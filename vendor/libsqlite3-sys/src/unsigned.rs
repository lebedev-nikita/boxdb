//! Exact fixed-width unsigned arithmetic. Runtime values retain their width.
use crate::{
    decimal::{Error, Result},
    native_types::{Kind, TYPES},
};
use num_traits::{FromPrimitive, ToPrimitive};

pub fn is_unsigned(kind: i32) -> bool {
    (7..=10).contains(&kind)
}
pub fn limit(kind: Kind) -> u64 {
    match kind {
        Kind::Uint8 => u8::MAX.into(),
        Kind::Uint16 => u16::MAX.into(),
        Kind::Uint32 => u32::MAX.into(),
        Kind::Uint64 => u64::MAX,
        _ => unreachable!(),
    }
}
pub fn read(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
}
pub fn encode(kind: Kind, text: &str) -> Result<Vec<u8>> {
    let n = text
        .trim()
        .parse::<u64>()
        .map_err(|_| Error("invalid unsigned integer".into()))?;
    if n > limit(kind) {
        return Err(Error("unsigned integer overflow".into()));
    }
    let d = TYPES.iter().find(|d| d.kind == kind).unwrap();
    let mut bytes = vec![d.tag];
    bytes.extend_from_slice(&n.to_be_bytes()[8 - d.payload_size..]);
    Ok(bytes)
}
fn integer(kind: i32, bytes: &[u8]) -> Result<i128> {
    match kind {
        7..=10 => Ok(i128::from(read(bytes))),
        -1 => Ok(i128::from(i64::from_ne_bytes(bytes.try_into().unwrap()))),
        0 => std::str::from_utf8(bytes)
            .map_err(|e| Error(e.to_string()))?
            .trim()
            .parse::<i128>()
            .map_err(|_| Error("unsigned operation requires an exact integer".into())),
        1 => {
            let n = crate::decimal::parse(
                std::str::from_utf8(bytes).map_err(|e| Error(e.to_string()))?,
            )?;
            let i = n
                .to_i128()
                .ok_or_else(|| Error("unsigned integer overflow".into()))?;
            if n != bigdecimal::BigDecimal::from(i) {
                return Err(Error("fractional unsigned cast".into()));
            }
            Ok(i)
        }
        _ => Err(Error(
            "unsigned operation requires an exact integer; cast REAL explicitly".into(),
        )),
    }
}
pub fn evaluate(op: i32, target: i32, ka: i32, a: &[u8], kb: i32, b: &[u8]) -> Result<Vec<u8>> {
    let kind = if op == 0 || op == 8 {
        target
    } else {
        ka.max(kb)
    };
    let kind = TYPES
        .iter()
        .find(|d| d.kind as i32 == kind)
        .ok_or_else(|| Error("unknown unsigned type".into()))?
        .kind;
    let a = if (op == 0 || op == 8) && ka == -2 {
        let real = f64::from_ne_bytes(a.try_into().unwrap());
        let decimal = bigdecimal::BigDecimal::from_f64(real)
            .ok_or_else(|| Error("non-finite unsigned cast".into()))?;
        let n = decimal
            .to_i128()
            .ok_or_else(|| Error("unsigned integer overflow".into()))?;
        if decimal != bigdecimal::BigDecimal::from(n) {
            return Err(Error("fractional unsigned cast".into()));
        }
        n
    } else {
        integer(ka, a)?
    };
    if op == 0 || op == 8 {
        return encode(kind, &a.to_string());
    }
    let b = if op == 13 { 0 } else { integer(kb, b)? };
    let value = match op {
        1 => a.checked_add(b),
        2 => a.checked_sub(b),
        3 => a.checked_mul(b),
        4 | 5 if b == 0 => return Err(Error("unsigned division by zero".into())),
        4 => a.checked_div(b),
        5 => a.checked_rem(b),
        9..=13 if a < 0 || b < 0 => {
            return Err(Error(
                "unsigned bitwise operations require nonnegative operands".into(),
            ));
        }
        9 => Some(a & b),
        10 => Some(a | b),
        11 => Some(if b >= 64 {
            0
        } else {
            i128::from((a as u64).wrapping_shl(b as u32) & limit(kind))
        }),
        12 => Some(if b >= 64 { 0 } else { a >> b }),
        13 => Some(i128::from(limit(kind)) ^ a),
        _ => return Err(Error("unsupported unsigned operator".into())),
    }
    .ok_or_else(|| Error("unsigned integer overflow".into()))?;
    encode(kind, &value.to_string())
}
