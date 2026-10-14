//! Decimal envelopes and arithmetic linked into the patched SQLite engine.
use crate as ffi;
use bigdecimal::{BigDecimal, RoundingMode};
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{FromPrimitive, One, Signed, ToPrimitive, Zero};
use std::{ffi::c_char, str::FromStr};

#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

pub const PREFIX: &[u8] = b"\0BDd1";
const BLOB_PREFIX: &[u8] = b"\0BDb1";

pub fn escape_blob(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(b"\0BD") {
        [BLOB_PREFIX, bytes].concat()
    } else {
        bytes.to_vec()
    }
}

pub fn unescape_blob(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(BLOB_PREFIX) && bytes[BLOB_PREFIX.len()..].starts_with(b"\0BD") {
        bytes[BLOB_PREFIX.len()..].to_vec()
    } else {
        bytes
    }
}

pub fn parse(text: &str) -> Result<BigDecimal> {
    if text.len() > 8194 {
        return Err(Error("DECIMAL exceeds size limit".into()));
    }
    let number = BigDecimal::from_str(text)
        .map_err(|_| Error("invalid DECIMAL".into()))?
        .normalized();
    let (coefficient, scale) = number.as_bigint_and_exponent();
    if coefficient.abs().to_string().len() > 4096 || scale.unsigned_abs() > 4096 {
        return Err(Error("DECIMAL exceeds 4096-digit limit".into()));
    }
    Ok(number)
}

pub fn encode(text: &str) -> Result<Vec<u8>> {
    let text = parse(text)?.normalized().to_plain_string();
    let mut bytes = PREFIX.to_vec();
    bytes.extend_from_slice(text.as_bytes());
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes.strip_prefix(PREFIX)?).ok()?;
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    if unsigned.bytes().filter(|byte| *byte == b'.').count() > 1
        || !unsigned
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    parse(text).ok()?;
    Some(text)
}

fn exact_divide(left: BigDecimal, right: BigDecimal) -> Result<BigDecimal> {
    let (mut numerator, left_scale) = left.as_bigint_and_exponent();
    let (mut denominator, right_scale) = right.as_bigint_and_exponent();
    if denominator.is_zero() {
        return Err(Error("DECIMAL division by zero".into()));
    }
    let gcd = numerator.gcd(&denominator);
    numerator /= &gcd;
    denominator /= gcd;
    if denominator.is_negative() {
        numerator = -numerator;
        denominator = -denominator;
    }
    let mut powers = [0u32; 2];
    for (index, factor) in [2, 5].into_iter().enumerate() {
        let factor = BigInt::from(factor);
        while (&denominator % &factor).is_zero() {
            denominator /= &factor;
            powers[index] += 1;
        }
    }
    if !denominator.is_one() {
        return Err(Error("nonterminating DECIMAL division; use decimal_div with an explicit scale and rounding mode".into()));
    }
    let scale = powers[0].max(powers[1]);
    numerator *= BigInt::from(2).pow(scale - powers[0]);
    numerator *= BigInt::from(5).pow(scale - powers[1]);
    Ok(BigDecimal::new(
        numerator,
        left_scale - right_scale + i64::from(scale),
    ))
}

unsafe fn operand(kind: i32, data: *const c_char, length: i32) -> Result<BigDecimal> {
    // SAFETY: SQLite passes a readable buffer for the duration of the callback.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length as usize) };
    if kind == 3 {
        let raw: [u8; 8] = bytes.try_into().map_err(|_| Error("invalid REAL".into()))?;
        return BigDecimal::from_f64(f64::from_ne_bytes(raw))
            .ok_or_else(|| Error("non-finite REAL cannot be DECIMAL".into()));
    }
    parse(std::str::from_utf8(bytes).map_err(|_| Error("invalid DECIMAL encoding".into()))?)
}

pub(crate) unsafe fn output(text: &[u8], result: *mut *mut c_char, length: *mut i32) -> i32 {
    // SAFETY: the caller owns both output slots; SQLite owns the allocation.
    let buffer = unsafe { ffi::sqlite3_malloc64((text.len() + 1) as u64) }.cast::<c_char>();
    if buffer.is_null() {
        return ffi::SQLITE_NOMEM;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer.cast(), text.len());
        *buffer.add(text.len()) = 0;
        *result = buffer;
        *length = text.len() as i32;
    }
    ffi::SQLITE_OK
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_decimal_eval(
    operation: i32,
    kind_left: i32,
    left: *const c_char,
    left_len: i32,
    kind_right: i32,
    right: *const c_char,
    right_len: i32,
    result: *mut *mut c_char,
    result_len: *mut i32,
) -> i32 {
    let value = (|| -> Result<Vec<u8>> {
        if operation != 8 && (kind_left == 3 || (operation != 0 && kind_right == 3)) {
            return Err(Error(
                "DECIMAL arithmetic requires an explicit REAL conversion".into(),
            ));
        }
        let left = unsafe { operand(kind_left, left, left_len)? };
        let number = if operation == 0 || operation == 8 {
            left
        } else {
            let right = unsafe { operand(kind_right, right, right_len)? };
            match operation {
                1 => left + right,
                2 => left - right,
                3 => left * right,
                4 => exact_divide(left, right)?,
                5 => {
                    if right.is_zero() {
                        return Err(Error("DECIMAL remainder by zero".into()));
                    }
                    left % right
                }
                6 => {
                    let scale = right
                        .to_i64()
                        .filter(|scale| (0..=4096).contains(scale))
                        .ok_or_else(|| Error("invalid DECIMAL rounding scale".into()))?;
                    left.with_scale_round(scale, RoundingMode::HalfUp)
                }
                7 => left.abs(),
                _ => return Err(Error("unsupported DECIMAL operator".into())),
            }
        };
        let text = number.to_plain_string();
        if operation == 6 {
            parse(&text)?;
            let mut bytes = PREFIX.to_vec();
            bytes.extend_from_slice(text.as_bytes());
            Ok(bytes)
        } else {
            encode(&text)
        }
    })();
    match value {
        Ok(bytes) => unsafe { output(&bytes, result, result_len) },
        Err(error) => {
            let rc = unsafe { output(error.0.as_bytes(), result, result_len) };
            if rc == ffi::SQLITE_OK {
                ffi::SQLITE_ERROR
            } else {
                rc
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_decimal_compare(
    kind_left: i32,
    left: *const c_char,
    left_len: i32,
    kind_right: i32,
    right: *const c_char,
    right_len: i32,
) -> i32 {
    let left = unsafe { operand(kind_left, left, left_len) };
    let right = unsafe { operand(kind_right, right, right_len) };
    match (left, right) {
        (Ok(left), Ok(right)) => left.cmp(&right) as i32,
        _ => 0,
    }
}
