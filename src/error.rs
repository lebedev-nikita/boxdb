use rusqlite::ffi;

/// A BoxDB failure retaining its original cause and engine result codes.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    code: &'static str,
}

impl Error {
    pub fn new(message: String) -> Self {
        Self {
            message,
            source: None,
            code: "BOXDB_VALIDATION",
        }
    }

    pub(crate) fn sqlite(code: i32, message: String) -> Self {
        rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(message)).into()
    }

    /// Add operation context without discarding engine codes or the source.
    pub fn with_context(mut self, context: impl std::fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    /// The symbolic BoxDB failure code.
    pub fn code(&self) -> &'static str {
        self.extended_code()
            .map(result_code_name)
            .unwrap_or(self.code)
    }

    /// The primary engine result code, when the engine reported this failure.
    pub fn result_code(&self) -> Option<i32> {
        self.extended_code().map(|code| code & 0xff)
    }

    /// The full extended engine result code, without truncation.
    pub fn extended_code(&self) -> Option<i32> {
        match self.source.as_ref()?.downcast_ref::<rusqlite::Error>()? {
            rusqlite::Error::SqliteFailure(error, _) => Some(error.extended_code),
            rusqlite::Error::SqlInputError { error, .. } => Some(error.extended_code),
            _ => None,
        }
    }
}

impl Error {
    /// A validation failure retaining the original parser or conversion error.
    pub fn validation(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::from_source(error, "BOXDB_VALIDATION")
    }

    pub(crate) fn with_source(
        mut self,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(error));
        self
    }

    fn from_source(
        error: impl std::error::Error + Send + Sync + 'static,
        code: &'static str,
    ) -> Self {
        Self {
            message: error.to_string(),
            source: Some(Box::new(error)),
            code,
        }
    }

    pub(crate) fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }

    /// A failure in the native client or engine infrastructure.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(message.into()).with_code("BOXDB_INTERNAL")
    }

    /// An operation attempted on a closed database or statement.
    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(message.into()).with_code("BOXDB_CLOSED")
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        let code = match error {
            rusqlite::Error::InvalidParameterName(_)
            | rusqlite::Error::InvalidParameterCount(..)
            | rusqlite::Error::InvalidQuery
            | rusqlite::Error::MultipleStatement
            | rusqlite::Error::InvalidPath(_)
            | rusqlite::Error::NulError(_) => "BOXDB_VALIDATION",
            _ => "BOXDB_ERROR",
        };
        Self::from_source(error, code)
    }
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Details<'a> {
            message: &'a str,
            code: &'static str,
            result_code: Option<i32>,
            extended_code: Option<i32>,
        }
        Details {
            message: &self.message,
            code: self.code(),
            result_code: self.result_code(),
            extended_code: self.extended_code(),
        }
        .serialize(serializer)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::from_source(error, "BOXDB_IO")
    }
}
impl From<serde_saphyr::SerializeError> for Error {
    fn from(error: serde_saphyr::SerializeError) -> Self {
        Self::from_source(error, "BOXDB_INTERNAL")
    }
}
macro_rules! convert_error {
    ($($ty:ty),*) => {$(impl From<$ty> for Error {
        fn from(error: $ty) -> Self { Self::from_source(error, "BOXDB_VALIDATION") }
    })*};
}
convert_error!(
    serde_json::Error,
    serde_saphyr::DeserializeError,
    sqlparser::parser::ParserError
);

// Map embedded engine result codes to BoxDB's public names.
fn result_code_name(code: i32) -> &'static str {
    macro_rules! names {
        ($($name:ident => $suffix:literal),* $(,)?) => {
            match code {
                $(ffi::$name => concat!("BOXDB_", $suffix),)*
                _ if code != code & 0xff => result_code_name(code & 0xff),
                _ => "BOXDB_ERROR",
            }
        };
    }
    names!(
        SQLITE_ERROR => "ERROR",
        SQLITE_INTERNAL => "INTERNAL",
        SQLITE_PERM => "PERM",
        SQLITE_ABORT => "ABORT",
        SQLITE_BUSY => "BUSY",
        SQLITE_LOCKED => "LOCKED",
        SQLITE_NOMEM => "NOMEM",
        SQLITE_READONLY => "READONLY",
        SQLITE_INTERRUPT => "INTERRUPT",
        SQLITE_IOERR => "IOERR",
        SQLITE_CORRUPT => "CORRUPT",
        SQLITE_NOTFOUND => "NOTFOUND",
        SQLITE_FULL => "FULL",
        SQLITE_CANTOPEN => "CANTOPEN",
        SQLITE_PROTOCOL => "PROTOCOL",
        SQLITE_EMPTY => "EMPTY",
        SQLITE_SCHEMA => "SCHEMA",
        SQLITE_TOOBIG => "TOOBIG",
        SQLITE_CONSTRAINT => "CONSTRAINT",
        SQLITE_MISMATCH => "MISMATCH",
        SQLITE_MISUSE => "MISUSE",
        SQLITE_NOLFS => "NOLFS",
        SQLITE_AUTH => "AUTH",
        SQLITE_FORMAT => "FORMAT",
        SQLITE_RANGE => "RANGE",
        SQLITE_NOTADB => "NOTADB",
        SQLITE_NOTICE => "NOTICE",
        SQLITE_WARNING => "WARNING",
        SQLITE_ERROR_MISSING_COLLSEQ => "ERROR_MISSING_COLLSEQ",
        SQLITE_ERROR_RETRY => "ERROR_RETRY",
        SQLITE_ERROR_SNAPSHOT => "ERROR_SNAPSHOT",
        SQLITE_ERROR_RESERVESIZE => "ERROR_RESERVESIZE",
        SQLITE_ERROR_KEY => "ERROR_KEY",
        SQLITE_ERROR_UNABLE => "ERROR_UNABLE",
        SQLITE_IOERR_READ => "IOERR_READ",
        SQLITE_IOERR_SHORT_READ => "IOERR_SHORT_READ",
        SQLITE_IOERR_WRITE => "IOERR_WRITE",
        SQLITE_IOERR_FSYNC => "IOERR_FSYNC",
        SQLITE_IOERR_DIR_FSYNC => "IOERR_DIR_FSYNC",
        SQLITE_IOERR_TRUNCATE => "IOERR_TRUNCATE",
        SQLITE_IOERR_FSTAT => "IOERR_FSTAT",
        SQLITE_IOERR_UNLOCK => "IOERR_UNLOCK",
        SQLITE_IOERR_RDLOCK => "IOERR_RDLOCK",
        SQLITE_IOERR_DELETE => "IOERR_DELETE",
        SQLITE_IOERR_BLOCKED => "IOERR_BLOCKED",
        SQLITE_IOERR_NOMEM => "IOERR_NOMEM",
        SQLITE_IOERR_ACCESS => "IOERR_ACCESS",
        SQLITE_IOERR_CHECKRESERVEDLOCK => "IOERR_CHECKRESERVEDLOCK",
        SQLITE_IOERR_LOCK => "IOERR_LOCK",
        SQLITE_IOERR_CLOSE => "IOERR_CLOSE",
        SQLITE_IOERR_DIR_CLOSE => "IOERR_DIR_CLOSE",
        SQLITE_IOERR_SHMOPEN => "IOERR_SHMOPEN",
        SQLITE_IOERR_SHMSIZE => "IOERR_SHMSIZE",
        SQLITE_IOERR_SHMLOCK => "IOERR_SHMLOCK",
        SQLITE_IOERR_SHMMAP => "IOERR_SHMMAP",
        SQLITE_IOERR_SEEK => "IOERR_SEEK",
        SQLITE_IOERR_DELETE_NOENT => "IOERR_DELETE_NOENT",
        SQLITE_IOERR_MMAP => "IOERR_MMAP",
        SQLITE_IOERR_GETTEMPPATH => "IOERR_GETTEMPPATH",
        SQLITE_IOERR_CONVPATH => "IOERR_CONVPATH",
        SQLITE_IOERR_VNODE => "IOERR_VNODE",
        SQLITE_IOERR_AUTH => "IOERR_AUTH",
        SQLITE_IOERR_BEGIN_ATOMIC => "IOERR_BEGIN_ATOMIC",
        SQLITE_IOERR_COMMIT_ATOMIC => "IOERR_COMMIT_ATOMIC",
        SQLITE_IOERR_ROLLBACK_ATOMIC => "IOERR_ROLLBACK_ATOMIC",
        SQLITE_IOERR_DATA => "IOERR_DATA",
        SQLITE_IOERR_CORRUPTFS => "IOERR_CORRUPTFS",
        SQLITE_IOERR_IN_PAGE => "IOERR_IN_PAGE",
        SQLITE_IOERR_BADKEY => "IOERR_BADKEY",
        SQLITE_IOERR_CODEC => "IOERR_CODEC",
        SQLITE_LOCKED_SHAREDCACHE => "LOCKED_SHAREDCACHE",
        SQLITE_LOCKED_VTAB => "LOCKED_VTAB",
        SQLITE_BUSY_RECOVERY => "BUSY_RECOVERY",
        SQLITE_BUSY_SNAPSHOT => "BUSY_SNAPSHOT",
        SQLITE_BUSY_TIMEOUT => "BUSY_TIMEOUT",
        SQLITE_CANTOPEN_NOTEMPDIR => "CANTOPEN_NOTEMPDIR",
        SQLITE_CANTOPEN_ISDIR => "CANTOPEN_ISDIR",
        SQLITE_CANTOPEN_FULLPATH => "CANTOPEN_FULLPATH",
        SQLITE_CANTOPEN_CONVPATH => "CANTOPEN_CONVPATH",
        SQLITE_CANTOPEN_DIRTYWAL => "CANTOPEN_DIRTYWAL",
        SQLITE_CANTOPEN_SYMLINK => "CANTOPEN_SYMLINK",
        SQLITE_CORRUPT_VTAB => "CORRUPT_VTAB",
        SQLITE_CORRUPT_SEQUENCE => "CORRUPT_SEQUENCE",
        SQLITE_CORRUPT_INDEX => "CORRUPT_INDEX",
        SQLITE_READONLY_RECOVERY => "READONLY_RECOVERY",
        SQLITE_READONLY_CANTLOCK => "READONLY_CANTLOCK",
        SQLITE_READONLY_ROLLBACK => "READONLY_ROLLBACK",
        SQLITE_READONLY_DBMOVED => "READONLY_DBMOVED",
        SQLITE_READONLY_CANTINIT => "READONLY_CANTINIT",
        SQLITE_READONLY_DIRECTORY => "READONLY_DIRECTORY",
        SQLITE_ABORT_ROLLBACK => "ABORT_ROLLBACK",
        SQLITE_CONSTRAINT_CHECK => "CONSTRAINT_CHECK",
        SQLITE_CONSTRAINT_COMMITHOOK => "CONSTRAINT_COMMITHOOK",
        SQLITE_CONSTRAINT_FOREIGNKEY => "CONSTRAINT_FOREIGNKEY",
        SQLITE_CONSTRAINT_FUNCTION => "CONSTRAINT_FUNCTION",
        SQLITE_CONSTRAINT_NOTNULL => "CONSTRAINT_NOTNULL",
        SQLITE_CONSTRAINT_PRIMARYKEY => "CONSTRAINT_PRIMARYKEY",
        SQLITE_CONSTRAINT_TRIGGER => "CONSTRAINT_TRIGGER",
        SQLITE_CONSTRAINT_UNIQUE => "CONSTRAINT_UNIQUE",
        SQLITE_CONSTRAINT_VTAB => "CONSTRAINT_VTAB",
        SQLITE_CONSTRAINT_ROWID => "CONSTRAINT_ROWID",
        SQLITE_CONSTRAINT_PINNED => "CONSTRAINT_PINNED",
        SQLITE_CONSTRAINT_DATATYPE => "CONSTRAINT_DATATYPE",
        SQLITE_NOTICE_RECOVER_WAL => "NOTICE_RECOVER_WAL",
        SQLITE_NOTICE_RECOVER_ROLLBACK => "NOTICE_RECOVER_ROLLBACK",
        SQLITE_NOTICE_RBU => "NOTICE_RBU",
        SQLITE_WARNING_AUTOINDEX => "WARNING_AUTOINDEX",
        SQLITE_AUTH_USER => "AUTH_USER",
    )
}
