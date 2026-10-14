//! Calendar fields and exact microsecond arithmetic; no implicit local timezone.
use crate::{
    decimal::{Error, Result},
    native::Kind,
};
use bigdecimal::{BigDecimal, RoundingMode};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Timelike, Utc};
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

pub(crate) const SECOND: i128 = 1_000_000;
pub(crate) const DAY: i128 = 86_400 * SECOND;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Interval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

fn invalid() -> Error {
    Error("invalid or overflowing temporal value".into())
}
pub(crate) fn checked_date(date: NaiveDate) -> Result<NaiveDate> {
    if !(0..=9999).contains(&date.year()) {
        return Err(invalid());
    }
    Ok(date)
}
fn rounded_datetime(value: NaiveDateTime) -> Result<NaiveDateTime> {
    let nanos = value.nanosecond();
    let mut micros = nanos / 1_000;
    let remainder = nanos % 1_000;
    if remainder > 500 || (remainder == 500 && micros % 2 == 1) {
        micros += 1;
    }
    let rounded = value
        .with_nanosecond(0)
        .ok_or_else(invalid)?
        .checked_add_signed(chrono::Duration::microseconds(i64::from(micros)))
        .ok_or_else(invalid)?;
    checked_date(rounded.date())?;
    Ok(rounded)
}
pub(crate) fn date(text: &str) -> Result<NaiveDate> {
    if text.len() != 10 {
        return Err(invalid());
    }
    checked_date(NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| invalid())?)
}
fn precision(text: &str) -> Result<()> {
    if text
        .split_once('.')
        .is_some_and(|(_, s)| s.bytes().take_while(u8::is_ascii_digit).count() > 9)
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn datetime(text: &str) -> Result<NaiveDateTime> {
    precision(text)?;
    let value = NaiveDateTime::parse_from_str(&text.replace(' ', "T"), "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| invalid())?;
    checked_date(value.date())?;
    if value.nanosecond() >= 1_000_000_000 {
        return Err(invalid());
    }
    rounded_datetime(value)
}
pub(crate) fn instant(text: &str) -> Result<DateTime<Utc>> {
    precision(text)?;
    let value = DateTime::parse_from_rfc3339(&text.replace(' ', "T"))
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    checked_date(value.date_naive())?;
    if value.nanosecond() >= 1_000_000_000 {
        return Err(invalid());
    }
    Ok(rounded_datetime(value.naive_utc())?.and_utc())
}

fn rounded_int(value: &BigDecimal) -> Result<i64> {
    value
        .with_scale_round(0, RoundingMode::HalfEven)
        .to_i64()
        .ok_or_else(invalid)
}
fn add_quantity(
    result: &mut Interval,
    time: &mut BigDecimal,
    value: BigDecimal,
    unit: &str,
) -> Result<()> {
    let (months, days, micros) = match unit.to_ascii_lowercase().as_str() {
        "year" | "years" | "yr" | "yrs" => (value * 12, BigDecimal::zero(), BigDecimal::zero()),
        "month" | "months" | "mon" | "mons" => (value, BigDecimal::zero(), BigDecimal::zero()),
        "week" | "weeks" => (BigDecimal::zero(), value * 7, BigDecimal::zero()),
        "day" | "days" => (BigDecimal::zero(), value, BigDecimal::zero()),
        "hour" | "hours" | "hr" | "hrs" => (
            BigDecimal::zero(),
            BigDecimal::zero(),
            value * 3_600_000_000i64,
        ),
        "minute" | "minutes" | "min" | "mins" => (
            BigDecimal::zero(),
            BigDecimal::zero(),
            value * 60_000_000i64,
        ),
        "second" | "seconds" | "sec" | "secs" => {
            (BigDecimal::zero(), BigDecimal::zero(), value * 1_000_000i64)
        }
        "millisecond" | "milliseconds" | "ms" => {
            (BigDecimal::zero(), BigDecimal::zero(), value * 1_000i64)
        }
        "microsecond" | "microseconds" | "us" => (BigDecimal::zero(), BigDecimal::zero(), value),
        "nanosecond" | "nanoseconds" | "ns" => {
            (BigDecimal::zero(), BigDecimal::zero(), value / 1_000i64)
        }
        _ => return Err(Error("invalid INTERVAL unit".into())),
    };
    let whole_months = months.to_i64().ok_or_else(invalid)?;
    let days = days + (months - BigDecimal::from(whole_months)) * 30i64;
    let whole_days = days.to_i64().ok_or_else(invalid)?;
    let micros = micros + (days - BigDecimal::from(whole_days)) * BigDecimal::from(DAY);
    result.months = i32::try_from(
        i64::from(result.months)
            .checked_add(whole_months)
            .ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())?;
    result.days = i32::try_from(
        i64::from(result.days)
            .checked_add(whole_days)
            .ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())?;
    *time += micros;
    Ok(())
}
pub(crate) fn interval(text: &str) -> Result<Interval> {
    if text.is_empty() || text.len() > 4096 {
        return Err(invalid());
    }
    let mut result = Interval {
        months: 0,
        days: 0,
        micros: 0,
    };
    let mut time_value = BigDecimal::zero();
    let (text, sign) = if let Some(t) = text.strip_prefix("-P") {
        (t, -1)
    } else if let Some(t) = text.strip_prefix("+P").or_else(|| text.strip_prefix('P')) {
        (t, 1)
    } else {
        (text, 0)
    };
    if sign != 0 {
        let mut start = 0;
        let mut time = false;
        let mut seen = false;
        let mut last = 0;
        for (i, c) in text.char_indices() {
            if c == 'T' {
                if start != i || time || i + 1 == text.len() {
                    return Err(invalid());
                }
                time = true;
                start = i + 1;
                continue;
            }
            if c.is_ascii_alphabetic() {
                let q = crate::decimal::parse(&text[start..i])? * sign;
                let unit = match (time, c) {
                    (false, 'Y') => "years",
                    (false, 'M') => "months",
                    (false, 'W') => "weeks",
                    (false, 'D') => "days",
                    (true, 'H') => "hours",
                    (true, 'M') => "minutes",
                    (true, 'S') => "seconds",
                    _ => return Err(invalid()),
                };
                let rank = match (time, c) {
                    (false, 'Y') => 1,
                    (false, 'M') => 2,
                    (false, 'W') => 3,
                    (false, 'D') => 4,
                    (true, 'H') => 5,
                    (true, 'M') => 6,
                    (true, 'S') => 7,
                    _ => unreachable!(),
                };
                if rank <= last {
                    return Err(invalid());
                }
                last = rank;
                add_quantity(&mut result, &mut time_value, q, unit)?;
                start = i + 1;
                seen = true;
            }
        }
        if !seen || start != text.len() {
            return Err(invalid());
        }
    } else {
        let parts: Vec<_> = text.split_whitespace().collect();
        let ago = parts.last() == Some(&"ago");
        let count = parts.len() - usize::from(ago);
        let mut i = 0;
        while i < count {
            if parts[i].contains(':') {
                let (clock, negative) = parts[i]
                    .strip_prefix('-')
                    .map_or((parts[i], false), |s| (s, true));
                let fields: Vec<_> = clock.trim_start_matches('+').split(':').collect();
                if fields.len() != 3 {
                    return Err(invalid());
                }
                let h = crate::decimal::parse(fields[0])?;
                let m = crate::decimal::parse(fields[1])?;
                let s = crate::decimal::parse(fields[2])?;
                if h < BigDecimal::zero()
                    || m < BigDecimal::zero()
                    || m >= BigDecimal::from(60)
                    || s < BigDecimal::zero()
                    || s >= BigDecimal::from(60)
                {
                    return Err(invalid());
                }
                let q = h * 3600i64 + m * 60i64 + s;
                add_quantity(
                    &mut result,
                    &mut time_value,
                    if negative { -q } else { q },
                    "seconds",
                )?;
                i += 1;
            } else {
                if i + 1 >= count {
                    return Err(invalid());
                }
                let q = crate::decimal::parse(parts[i])?;
                add_quantity(&mut result, &mut time_value, q, parts[i + 1])?;
                i += 2;
            }
        }
        if count == 0 {
            return Err(invalid());
        }
        if ago {
            result.months = result.months.checked_neg().ok_or_else(invalid)?;
            result.days = result.days.checked_neg().ok_or_else(invalid)?;
            time_value = -time_value;
        }
    }
    result.micros = rounded_int(&time_value)?;
    Ok(result)
}
pub(crate) fn interval_text(value: Interval) -> String {
    let micros = i128::from(value.micros).abs();
    let seconds = micros / SECOND;
    format!(
        "{} mons {} days {}{:02}:{:02}:{:02}.{:06}",
        value.months,
        value.days,
        if value.micros < 0 { "-" } else { "" },
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        micros % SECOND
    )
}
pub(crate) fn interval_bytes(value: Interval) -> Vec<u8> {
    [
        value.months.to_be_bytes().as_slice(),
        value.days.to_be_bytes().as_slice(),
        value.micros.to_be_bytes().as_slice(),
    ]
    .concat()
}
pub(crate) fn read_interval(bytes: &[u8]) -> Result<Interval> {
    if bytes.len() != 16 {
        return Err(invalid());
    }
    Ok(Interval {
        months: i32::from_be_bytes(bytes[..4].try_into().unwrap()),
        days: i32::from_be_bytes(bytes[4..8].try_into().unwrap()),
        micros: i64::from_be_bytes(bytes[8..].try_into().unwrap()),
    })
}
pub(crate) fn read_datetime(bytes: &[u8]) -> Result<NaiveDateTime> {
    if bytes.len() != 12 {
        return Err(invalid());
    }
    let seconds = i64::from_be_bytes(bytes[..8].try_into().unwrap());
    let micros = u32::from_be_bytes(bytes[8..].try_into().unwrap());
    if micros >= 1_000_000 {
        return Err(invalid());
    }
    let value = DateTime::from_timestamp(seconds, micros * 1_000)
        .ok_or_else(invalid)?
        .naive_utc();
    checked_date(value.date())?;
    Ok(value)
}
pub(crate) fn read_date(bytes: &[u8]) -> Result<NaiveDate> {
    let days = i32::from_be_bytes(bytes.try_into().map_err(|_| invalid())?);
    checked_date(NaiveDate::from_num_days_from_ce_opt(days).ok_or_else(invalid)?)
}
pub(crate) fn datetime_bytes(value: NaiveDateTime) -> Vec<u8> {
    [
        value.and_utc().timestamp().to_be_bytes().as_slice(),
        (value.nanosecond() / 1_000).to_be_bytes().as_slice(),
    ]
    .concat()
}
pub(crate) fn encode(kind: Kind, text: &str) -> Result<Vec<u8>> {
    match kind {
        Kind::Date => Ok(date(text)?.num_days_from_ce().to_be_bytes().to_vec()),
        Kind::DateTime => Ok(datetime_bytes(datetime(text)?)),
        Kind::Instant => Ok(datetime_bytes(instant(text)?.naive_utc())),
        Kind::Interval => Ok(interval_bytes(interval(text)?)),
        _ => Err(invalid()),
    }
}
pub(crate) fn text(kind: Kind, bytes: &[u8]) -> Result<String> {
    match kind {
        Kind::Date => Ok(read_date(bytes)?.format("%Y-%m-%d").to_string()),
        Kind::DateTime => Ok(read_datetime(bytes)?
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string()),
        Kind::Instant => Ok(read_datetime(bytes)?
            .and_utc()
            .to_rfc3339_opts(SecondsFormat::Micros, true)),
        Kind::Interval => Ok(interval_text(read_interval(bytes)?)),
        _ => Err(invalid()),
    }
}

pub(crate) fn cast(target: Kind, source: Kind, bytes: &[u8], assignment: bool) -> Result<Vec<u8>> {
    if target == source {
        text(source, bytes)?;
        return Ok(bytes.to_vec());
    }
    let value = match source {
        Kind::Date => read_date(bytes)?.and_hms_opt(0, 0, 0).unwrap(),
        Kind::DateTime | Kind::Instant => read_datetime(bytes)?,
        _ => return Err(Error("unsupported native conversion".into())),
    };
    if assignment
        && !((source == Kind::Date && target == Kind::DateTime)
            || (source == Kind::DateTime
                && target == Kind::Date
                && value.time() == chrono::NaiveTime::MIN))
    {
        return Err(Error("native assignment requires an explicit cast".into()));
    }
    match target {
        Kind::Date => Ok(value.date().num_days_from_ce().to_be_bytes().to_vec()),
        Kind::DateTime | Kind::Instant => Ok(datetime_bytes(value)),
        _ => Err(Error("unsupported native conversion".into())),
    }
}

pub(crate) fn calendar_add(value: NaiveDateTime, interval: Interval) -> Result<NaiveDateTime> {
    let months = chrono::Months::new(interval.months.unsigned_abs());
    let date = if interval.months < 0 {
        value.date().checked_sub_months(months)
    } else {
        value.date().checked_add_months(months)
    }
    .ok_or_else(invalid)?;
    let value = date
        .and_time(value.time())
        .checked_add_signed(chrono::Duration::days(i64::from(interval.days)))
        .ok_or_else(invalid)?
        .checked_add_signed(chrono::Duration::microseconds(interval.micros))
        .ok_or_else(invalid)?;
    checked_date(value.date())?;
    Ok(value)
}

pub(crate) fn operation(
    operation: i32,
    kind_a: i32,
    a: &[u8],
    kind_b: i32,
    b: &[u8],
) -> Result<(i32, Vec<u8>)> {
    if operation == 3 && kind_b == Kind::Interval as i32 && kind_a != Kind::Interval as i32 {
        return self::operation(operation, kind_b, b, kind_a, a);
    }
    if (operation == 1 || operation == 2)
        && kind_a == Kind::Interval as i32
        && kind_b == Kind::Interval as i32
    {
        let left = read_interval(a)?;
        let mut right = read_interval(b)?;
        if operation == 2 {
            right = negate(right)?;
        }
        return Ok((
            Kind::Interval as i32,
            interval_bytes(Interval {
                months: left.months.checked_add(right.months).ok_or_else(invalid)?,
                days: left.days.checked_add(right.days).ok_or_else(invalid)?,
                micros: left.micros.checked_add(right.micros).ok_or_else(invalid)?,
            }),
        ));
    }
    if (operation == 3 || operation == 4) && kind_a == Kind::Interval as i32 {
        let factor = scalar(kind_b, b)?;
        if operation == 4 && factor.is_zero() {
            return Err(Error("INTERVAL division by zero".into()));
        }
        let value = read_interval(a)?;
        let (coefficient, scale) = factor.as_bigint_and_exponent();
        let (mut numerator, mut denominator) = if scale >= 0 {
            (coefficient, BigInt::from(10).pow(scale as u32))
        } else {
            (
                coefficient * BigInt::from(10).pow((-scale) as u32),
                BigInt::from(1),
            )
        };
        if operation == 4 {
            std::mem::swap(&mut numerator, &mut denominator);
        }
        if denominator.is_negative() {
            numerator = -numerator;
            denominator = -denominator;
        }
        let (months, month_fraction) =
            (BigInt::from(value.months) * &numerator).div_rem(&denominator);
        let (days, day_fraction) =
            (BigInt::from(value.days) * &numerator + month_fraction * 30i32).div_rem(&denominator);
        let (mut micros, remainder) = (BigInt::from(value.micros) * numerator
            + day_fraction * BigInt::from(DAY))
        .div_rem(&denominator);
        let twice = remainder.abs() * 2;
        if twice > denominator || (twice == denominator && micros.is_odd()) {
            micros += if remainder.is_negative() { -1 } else { 1 };
        }
        return Ok((
            Kind::Interval as i32,
            interval_bytes(Interval {
                months: months.to_i32().ok_or_else(invalid)?,
                days: days.to_i32().ok_or_else(invalid)?,
                micros: micros.to_i64().ok_or_else(invalid)?,
            }),
        ));
    }
    if operation == 2
        && kind_a == -1
        && scalar(kind_a, a)?.is_zero()
        && kind_b == Kind::Interval as i32
    {
        return Ok((
            Kind::Interval as i32,
            interval_bytes(negate(read_interval(b)?)?),
        ));
    }
    if operation == 1 && kind_a == Kind::Interval as i32 && matches!(kind_b, 3 | 4 | 5) {
        return self::operation(operation, kind_b, b, kind_a, a);
    }
    if (operation == 1 || operation == 2)
        && matches!(kind_a, 3 | 4 | 5)
        && kind_b == Kind::Interval as i32
    {
        let kind = if kind_a == Kind::Instant as i32 {
            Kind::Instant
        } else {
            Kind::DateTime
        };
        let value = if kind_a == Kind::Date as i32 {
            read_date(a)?.and_hms_opt(0, 0, 0).unwrap()
        } else {
            read_datetime(a)?
        };
        let interval = read_interval(b)?;
        let interval = if operation == 2 {
            negate(interval)?
        } else {
            interval
        };
        return Ok((kind as i32, datetime_bytes(calendar_add(value, interval)?)));
    }
    if operation == 2
        && (kind_a == Kind::Instant as i32 || kind_a == Kind::DateTime as i32)
        && kind_a == kind_b
    {
        let left = read_datetime(a)?.and_utc();
        let right = read_datetime(b)?.and_utc();
        let delta = (i128::from(left.timestamp()) - i128::from(right.timestamp())) * SECOND
            + i128::from(left.nanosecond() / 1_000)
            - i128::from(right.nanosecond() / 1_000);
        let result = Interval {
            months: 0,
            days: i32::try_from(delta / DAY).map_err(|_| invalid())?,
            micros: i64::try_from(delta % DAY).map_err(|_| invalid())?,
        };
        return Ok((Kind::Interval as i32, interval_bytes(result)));
    }
    if operation == 1 && kind_a == -1 && kind_b == Kind::Date as i32 {
        return self::operation(operation, kind_b, b, kind_a, a);
    }
    if (operation == 1 || operation == 2) && kind_a == Kind::Date as i32 && kind_b == -1 {
        let days = i64::from_ne_bytes(b.try_into().map_err(|_| invalid())?);
        let days = if operation == 2 {
            days.checked_neg().ok_or_else(invalid)?
        } else {
            days
        };
        let delta = chrono::Duration::try_days(days).ok_or_else(invalid)?;
        let date = checked_date(
            read_date(a)?
                .checked_add_signed(delta)
                .ok_or_else(invalid)?,
        )?;
        return Ok((
            Kind::Date as i32,
            date.num_days_from_ce().to_be_bytes().to_vec(),
        ));
    }
    if operation == 2 && kind_a == Kind::Date as i32 && kind_b == Kind::Date as i32 {
        let days = read_date(a)?
            .signed_duration_since(read_date(b)?)
            .num_days();
        return Ok((-1, days.to_ne_bytes().to_vec()));
    }
    Err(Error("unsupported native operator or operand types".into()))
}

pub(crate) fn negate(value: Interval) -> Result<Interval> {
    Ok(Interval {
        months: value.months.checked_neg().ok_or_else(invalid)?,
        days: value.days.checked_neg().ok_or_else(invalid)?,
        micros: value.micros.checked_neg().ok_or_else(invalid)?,
    })
}
pub(crate) fn scalar(kind: i32, bytes: &[u8]) -> Result<BigDecimal> {
    match kind {
        -1 => Ok(BigDecimal::from(i64::from_ne_bytes(
            bytes.try_into().map_err(|_| invalid())?,
        ))),
        -2 => BigDecimal::from_f64(f64::from_ne_bytes(bytes.try_into().map_err(|_| invalid())?))
            .ok_or_else(invalid),
        0 | 1 => crate::decimal::parse(std::str::from_utf8(bytes).map_err(|_| invalid())?),
        _ => Err(Error("INTERVAL scaling requires a numeric operand".into())),
    }
}
