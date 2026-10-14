use rusqlite::ffi;

/// A BoxDB failure retaining SQLite's original error and result codes.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    #[source]
    source: Option<rusqlite::Error>,
}

impl Error {
    pub fn new(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }

    pub(crate) fn sqlite(code: i32, message: String) -> Self {
        rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(message)).into()
    }

    /// Add operation context without discarding SQLite codes or the source.
    pub fn with_context(mut self, context: impl std::fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    /// The symbolic SQLite result code, or BOXDB_ERROR for other failures.
    pub fn code(&self) -> &'static str {
        self.sqlite_extended_code()
            .map(sqlite_code_name)
            .unwrap_or("BOXDB_ERROR")
    }

    /// SQLite's primary result code, when SQLite reported this failure.
    pub fn sqlite_code(&self) -> Option<i32> {
        self.sqlite_extended_code().map(|code| code & 0xff)
    }

    /// SQLite's full extended result code, without truncation.
    pub fn sqlite_extended_code(&self) -> Option<i32> {
        match self.source.as_ref()? {
            rusqlite::Error::SqliteFailure(error, _) => Some(error.extended_code),
            rusqlite::Error::SqlInputError { error, .. } => Some(error.extended_code),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            message: error.to_string(),
            source: Some(error),
        }
    }
}

macro_rules! convert_error {
    ($($ty:ty),*) => {$(impl From<$ty> for Error {
        fn from(error: $ty) -> Self { Self::new(error.to_string()) }
    })*};
}
convert_error!(
    std::io::Error,
    serde_json::Error,
    serde_saphyr::DeserializeError,
    serde_saphyr::SerializeError,
    sqlparser::parser::ParserError
);

// Use SQLite's symbolic names rather than rusqlite's ErrorCode Debug names.
fn sqlite_code_name(code: i32) -> &'static str {
    macro_rules! names {
        ($($name:ident),* $(,)?) => {
            match code {
                $(ffi::$name => stringify!($name),)*
                _ if code != code & 0xff => sqlite_code_name(code & 0xff),
                _ => "SQLITE_ERROR",
            }
        };
    }
    names!(
        SQLITE_ERROR,
        SQLITE_INTERNAL,
        SQLITE_PERM,
        SQLITE_ABORT,
        SQLITE_BUSY,
        SQLITE_LOCKED,
        SQLITE_NOMEM,
        SQLITE_READONLY,
        SQLITE_INTERRUPT,
        SQLITE_IOERR,
        SQLITE_CORRUPT,
        SQLITE_NOTFOUND,
        SQLITE_FULL,
        SQLITE_CANTOPEN,
        SQLITE_PROTOCOL,
        SQLITE_EMPTY,
        SQLITE_SCHEMA,
        SQLITE_TOOBIG,
        SQLITE_CONSTRAINT,
        SQLITE_MISMATCH,
        SQLITE_MISUSE,
        SQLITE_NOLFS,
        SQLITE_AUTH,
        SQLITE_FORMAT,
        SQLITE_RANGE,
        SQLITE_NOTADB,
        SQLITE_NOTICE,
        SQLITE_WARNING,
        SQLITE_ERROR_MISSING_COLLSEQ,
        SQLITE_ERROR_RETRY,
        SQLITE_ERROR_SNAPSHOT,
        SQLITE_ERROR_RESERVESIZE,
        SQLITE_ERROR_KEY,
        SQLITE_ERROR_UNABLE,
        SQLITE_IOERR_READ,
        SQLITE_IOERR_SHORT_READ,
        SQLITE_IOERR_WRITE,
        SQLITE_IOERR_FSYNC,
        SQLITE_IOERR_DIR_FSYNC,
        SQLITE_IOERR_TRUNCATE,
        SQLITE_IOERR_FSTAT,
        SQLITE_IOERR_UNLOCK,
        SQLITE_IOERR_RDLOCK,
        SQLITE_IOERR_DELETE,
        SQLITE_IOERR_BLOCKED,
        SQLITE_IOERR_NOMEM,
        SQLITE_IOERR_ACCESS,
        SQLITE_IOERR_CHECKRESERVEDLOCK,
        SQLITE_IOERR_LOCK,
        SQLITE_IOERR_CLOSE,
        SQLITE_IOERR_DIR_CLOSE,
        SQLITE_IOERR_SHMOPEN,
        SQLITE_IOERR_SHMSIZE,
        SQLITE_IOERR_SHMLOCK,
        SQLITE_IOERR_SHMMAP,
        SQLITE_IOERR_SEEK,
        SQLITE_IOERR_DELETE_NOENT,
        SQLITE_IOERR_MMAP,
        SQLITE_IOERR_GETTEMPPATH,
        SQLITE_IOERR_CONVPATH,
        SQLITE_IOERR_VNODE,
        SQLITE_IOERR_AUTH,
        SQLITE_IOERR_BEGIN_ATOMIC,
        SQLITE_IOERR_COMMIT_ATOMIC,
        SQLITE_IOERR_ROLLBACK_ATOMIC,
        SQLITE_IOERR_DATA,
        SQLITE_IOERR_CORRUPTFS,
        SQLITE_IOERR_IN_PAGE,
        SQLITE_IOERR_BADKEY,
        SQLITE_IOERR_CODEC,
        SQLITE_LOCKED_SHAREDCACHE,
        SQLITE_LOCKED_VTAB,
        SQLITE_BUSY_RECOVERY,
        SQLITE_BUSY_SNAPSHOT,
        SQLITE_BUSY_TIMEOUT,
        SQLITE_CANTOPEN_NOTEMPDIR,
        SQLITE_CANTOPEN_ISDIR,
        SQLITE_CANTOPEN_FULLPATH,
        SQLITE_CANTOPEN_CONVPATH,
        SQLITE_CANTOPEN_DIRTYWAL,
        SQLITE_CANTOPEN_SYMLINK,
        SQLITE_CORRUPT_VTAB,
        SQLITE_CORRUPT_SEQUENCE,
        SQLITE_CORRUPT_INDEX,
        SQLITE_READONLY_RECOVERY,
        SQLITE_READONLY_CANTLOCK,
        SQLITE_READONLY_ROLLBACK,
        SQLITE_READONLY_DBMOVED,
        SQLITE_READONLY_CANTINIT,
        SQLITE_READONLY_DIRECTORY,
        SQLITE_ABORT_ROLLBACK,
        SQLITE_CONSTRAINT_CHECK,
        SQLITE_CONSTRAINT_COMMITHOOK,
        SQLITE_CONSTRAINT_FOREIGNKEY,
        SQLITE_CONSTRAINT_FUNCTION,
        SQLITE_CONSTRAINT_NOTNULL,
        SQLITE_CONSTRAINT_PRIMARYKEY,
        SQLITE_CONSTRAINT_TRIGGER,
        SQLITE_CONSTRAINT_UNIQUE,
        SQLITE_CONSTRAINT_VTAB,
        SQLITE_CONSTRAINT_ROWID,
        SQLITE_CONSTRAINT_PINNED,
        SQLITE_CONSTRAINT_DATATYPE,
        SQLITE_NOTICE_RECOVER_WAL,
        SQLITE_NOTICE_RECOVER_ROLLBACK,
        SQLITE_NOTICE_RBU,
        SQLITE_WARNING_AUTOINDEX,
        SQLITE_AUTH_USER,
    )
}
