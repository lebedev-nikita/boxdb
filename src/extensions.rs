//! Statically linked extensions are registered on BoxDB-owned connections.
use crate::{Error, Result};
use rusqlite::{Connection, ffi};
use std::ffi::{CStr, c_char, c_int};

type ExtensionInit = unsafe extern "C" fn(
    *mut ffi::sqlite3,
    *mut *mut c_char,
    *const ffi::sqlite3_api_routines,
) -> c_int;

unsafe extern "C" {
    fn text_init(db: *mut ffi::sqlite3) -> c_int;
    fn regexp_init(db: *mut ffi::sqlite3) -> c_int;
    fn sqlite3_decimal_init(
        db: *mut ffi::sqlite3,
        error: *mut *mut c_char,
        api: *const ffi::sqlite3_api_routines,
    ) -> c_int;
}

pub(crate) fn register(db: &Connection) -> Result<()> {
    for (name, init) in [
        ("text", text_init as unsafe extern "C" fn(_) -> _),
        ("regexp", regexp_init),
    ] {
        // SAFETY: the statically linked modules use this live SQLite connection.
        let code = unsafe { init(db.handle()) };
        if code != ffi::SQLITE_OK {
            return Err(Error::sqlite(
                code,
                format!("cannot register {name} extension: SQLite error {code}"),
            ));
        }
    }
    register_api(db, "decimal", sqlite3_decimal_init)?;
    // SAFETY: sqlite-vec's C entry point has ExtensionInit's signature; its
    // Rust crate declares an untyped entry point for callers to cast.
    let vector_init = unsafe {
        std::mem::transmute::<unsafe extern "C" fn(), ExtensionInit>(sqlite_vec::sqlite3_vec_init)
    };
    register_api(db, "vec", vector_init)
}

fn register_api(db: &Connection, name: &str, init: ExtensionInit) -> Result<()> {
    let mut message = std::ptr::null_mut();
    // SAFETY: the connection owns a live handle. SQLITE_CORE extensions call
    // SQLite directly, so they accept a null API table. SQLite owns error text.
    let code = unsafe { init(db.handle(), &mut message, std::ptr::null()) };
    let detail = if message.is_null() {
        format!("SQLite error {code}")
    } else {
        // SAFETY: extension errors are NUL-terminated and allocated by SQLite.
        unsafe {
            let detail = CStr::from_ptr(message).to_string_lossy().into_owned();
            ffi::sqlite3_free(message.cast());
            detail
        }
    };
    if code != ffi::SQLITE_OK {
        return Err(Error::sqlite(
            code,
            format!("cannot register {name} extension: {detail}"),
        ));
    }
    Ok(())
}
