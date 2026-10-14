#![expect(non_snake_case, non_camel_case_types)]

// force linking to openssl
#[cfg(feature = "bundled-sqlcipher-vendored-openssl")]
extern crate openssl_sys;

pub use self::error::*;

use core::mem;

mod error;
pub mod decimal;
pub mod native;
pub mod wire;
mod temporal;
mod temporal_functions;
#[path = "../native_types.rs"]
mod native_types;

#[must_use]
pub fn SQLITE_STATIC() -> sqlite3_destructor_type {
    None
}

#[must_use]
pub fn SQLITE_TRANSIENT() -> sqlite3_destructor_type {
    Some(unsafe { mem::transmute::<isize, unsafe extern "C" fn(*mut core::ffi::c_void)>(-1_isize) })
}

#[allow(dead_code, clippy::all)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindgen.rs"));
}
pub use bindings::*;

// Rust clients read storage cells for typed decoding. C extensions use the
// standard column accessors, which expose logical BLOB payloads.
unsafe extern "C" {
    #[link_name = "sqlite3_boxdb_column_blob"]
    pub fn sqlite3_column_blob(statement: *mut sqlite3_stmt, column: i32) -> *const core::ffi::c_void;
    #[link_name = "sqlite3_boxdb_column_bytes"]
    pub fn sqlite3_column_bytes(statement: *mut sqlite3_stmt, column: i32) -> i32;
    #[link_name = "sqlite3_boxdb_column_bytes16"]
    pub fn sqlite3_column_bytes16(statement: *mut sqlite3_stmt, column: i32) -> i32;
}

impl Default for sqlite3_vtab {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

impl Default for sqlite3_vtab_cursor {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}
