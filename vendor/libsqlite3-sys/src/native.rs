//! Native value codecs and operations behind SQLite's common type dispatch.
pub use crate::native_types::Kind;
use crate::{
    decimal::{Error, Result},
    native_types::TYPES,
};
use chrono::Datelike;
use std::ffi::c_char;
use std::hash::{Hash, Hasher};

pub fn encode(kind: Kind, text: &str) -> Result<Vec<u8>> {
    let payload = match kind {
        Kind::Uuid => uuid::Uuid::parse_str(text)
            .map_err(|e| Error(e.to_string()))?
            .as_bytes()
            .to_vec(),
        Kind::Decimal => return crate::decimal::encode(text),
        _ => crate::temporal::encode(kind, text)?,
    };
    let descriptor = TYPES.iter().find(|d| d.kind == kind).unwrap();
    let mut bytes = vec![descriptor.tag];
    bytes.extend(payload);
    Ok(bytes)
}

pub fn text(kind: Kind, bytes: &[u8]) -> Result<String> {
    match kind {
        Kind::Uuid => Ok(uuid::Uuid::from_bytes(
            bytes
                .try_into()
                .map_err(|_| Error("invalid UUID storage".into()))?,
        )
        .to_string()),
        Kind::Decimal => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|e| Error(e.to_string())),
        _ => crate::temporal::text(kind, bytes),
    }
}

pub fn decode(bytes: &[u8]) -> Option<(Kind, String)> {
    if bytes.is_empty() {
        return None;
    }
    let descriptor = TYPES
        .iter()
        .find(|d| d.tag == bytes[0] && d.kind != Kind::Decimal)?;
    if bytes.len() != 1 + descriptor.payload_size {
        return None;
    }
    Some((descriptor.kind, text(descriptor.kind, &bytes[1..]).ok()?))
}

pub fn canonical(kind: Kind, value: &str) -> Result<String> {
    let bytes = encode(kind, value)?;
    text(kind, &bytes[1..])
}

pub fn cast(target: Kind, source: Kind, value: &str) -> Result<String> {
    let bytes = encode(source, value)?;
    let payload = if target == source {
        bytes[1..].to_vec()
    } else {
        crate::temporal::cast(target, source, &bytes[1..], false)?
    };
    text(target, &payload)
}

#[repr(C)]
struct FunctionArgument {
    kind: i32,
    data: *const c_char,
    length: i32,
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_native_function(
    function: i32,
    argc: i32,
    args: *const FunctionArgument,
    out: *mut *mut c_char,
    out_len: *mut i32,
) -> i32 {
    let result = (|| {
        if !(0..=7).contains(&argc) {
            return Err(Error("invalid temporal function argument count".into()));
        }
        let args = unsafe { std::slice::from_raw_parts(args, argc as usize) }
            .iter()
            .map(|arg| crate::temporal_functions::Argument {
                kind: arg.kind,
                bytes: unsafe { input(arg.data, arg.length) },
            })
            .collect::<Vec<_>>();
        let (kind, payload) = crate::temporal_functions::evaluate(function, &args)?;
        let descriptor = TYPES.iter().find(|d| d.kind == kind).unwrap();
        let mut bytes = vec![descriptor.tag];
        bytes.extend(payload);
        Ok(bytes)
    })();
    match result {
        Ok(bytes) => unsafe { crate::decimal::output(&bytes, out, out_len) },
        Err(error) => {
            let rc = unsafe { crate::decimal::output(error.0.as_bytes(), out, out_len) };
            if rc == crate::SQLITE_OK {
                crate::SQLITE_ERROR
            } else {
                rc
            }
        }
    }
}

fn ordering_key(kind: i32, bytes: &[u8]) -> i128 {
    match kind {
        3 | 5 => {
            i128::from(i64::from_be_bytes(bytes[..8].try_into().unwrap())) * crate::temporal::SECOND
                + i128::from(u32::from_be_bytes(bytes[8..12].try_into().unwrap()))
        }
        4 => {
            (i128::from(i32::from_be_bytes(bytes[..4].try_into().unwrap()))
                - i128::from(
                    chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
                        .unwrap()
                        .num_days_from_ce(),
                ))
                * crate::temporal::DAY
        }
        6 => {
            let value = crate::temporal::read_interval(bytes).unwrap();
            (i128::from(value.months) * 30 + i128::from(value.days)) * crate::temporal::DAY
                + i128::from(value.micros)
        }
        _ => 0,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_native_compare(
    kind: i32,
    a: *const c_char,
    na: i32,
    kind_b: i32,
    b: *const c_char,
    nb: i32,
) -> i32 {
    let a = unsafe { input(a, na) };
    let b = unsafe { input(b, nb) };
    if kind == 2 {
        a.cmp(b) as i32
    } else {
        ordering_key(kind, a).cmp(&ordering_key(kind_b, b)) as i32
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_native_hash(kind: i32, data: *const c_char, length: i32) -> u64 {
    let bytes = unsafe { input(data, length) };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (if matches!(kind, 4 | 5) { 4 } else { kind }).hash(&mut hash);
    if matches!(kind, 4 | 5 | 6) {
        ordering_key(kind, bytes).hash(&mut hash);
    } else {
        bytes.hash(&mut hash);
    }
    hash.finish()
}

unsafe fn input<'a>(data: *const c_char, length: i32) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(data.cast(), length as usize) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_native_text(
    kind: i32,
    data: *const c_char,
    length: i32,
    out: *mut *mut c_char,
    out_len: *mut i32,
) -> i32 {
    let result = (|| {
        let kind = TYPES
            .iter()
            .find(|d| d.kind as i32 == kind)
            .ok_or_else(|| Error("unknown native type".into()))?
            .kind;
        text(kind, unsafe { input(data, length) })
    })();
    match result {
        Ok(value) => unsafe { crate::decimal::output(value.as_bytes(), out, out_len) },
        Err(error) => {
            let rc = unsafe { crate::decimal::output(error.0.as_bytes(), out, out_len) };
            if rc == crate::SQLITE_OK {
                crate::SQLITE_ERROR
            } else {
                rc
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn boxdb_native_eval(
    operation: i32,
    target: i32,
    kind_a: i32,
    data_a: *const c_char,
    len_a: i32,
    kind_b: i32,
    data_b: *const c_char,
    len_b: i32,
    out: *mut *mut c_char,
    out_len: *mut i32,
    out_kind: *mut i32,
) -> i32 {
    let result = (|| {
        if operation != 0 && operation != 8 {
            let (kind, payload) = crate::temporal::operation(
                operation,
                kind_a,
                unsafe { input(data_a, len_a) },
                kind_b,
                unsafe { input(data_b, len_b) },
            )?;
            unsafe {
                *out_kind = kind;
            }
            if kind < 0 {
                return Ok(payload);
            }
            let descriptor = TYPES.iter().find(|d| d.kind as i32 == kind).unwrap();
            let mut bytes = vec![descriptor.tag];
            bytes.extend(payload);
            return Ok(bytes);
        }
        let kind = TYPES
            .iter()
            .find(|d| d.kind as i32 == target)
            .ok_or_else(|| Error("unknown native type".into()))?
            .kind;
        unsafe {
            *out_kind = kind as i32;
        }
        if kind_a > 0 {
            let source = TYPES
                .iter()
                .find(|d| d.kind as i32 == kind_a)
                .ok_or_else(|| Error("unknown native type".into()))?
                .kind;
            let input = unsafe { input(data_a, len_a) };
            let payload = if kind == source {
                text(kind, input)?;
                input.to_vec()
            } else {
                crate::temporal::cast(kind, source, input, operation == 8)?
            };
            let descriptor = TYPES.iter().find(|d| d.kind == kind).unwrap();
            let mut bytes = vec![descriptor.tag];
            bytes.extend(payload);
            return Ok(bytes);
        }
        if kind_a != 0 {
            return Err(Error("unsupported native conversion".into()));
        }
        let text = std::str::from_utf8(unsafe { input(data_a, len_a) })
            .map_err(|e| Error(e.to_string()))?;
        encode(kind, text)
    })();
    match result {
        Ok(bytes) => unsafe { crate::decimal::output(&bytes, out, out_len) },
        Err(error) => {
            let rc = unsafe { crate::decimal::output(error.0.as_bytes(), out, out_len) };
            if rc == crate::SQLITE_OK {
                crate::SQLITE_ERROR
            } else {
                rc
            }
        }
    }
}
