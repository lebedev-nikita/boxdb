use boxdb::{Database, Error, Format, InterruptHandle, Value};
use serde::Deserialize;
use serde_json::json;
use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{self, Sender, SyncSender},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

type Request = (String, SyncSender<String>);

pub struct Handle {
    sender: Option<Sender<Request>>,
    worker: Option<JoinHandle<()>>,
    interrupt: InterruptHandle,
    cancelled: Arc<AtomicBool>,
}

thread_local! {
    static CANCELLATION: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

fn busy(attempt: i32) -> bool {
    let cancelled = || {
        CANCELLATION.with(|flag| {
            flag.borrow()
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
        })
    };
    if attempt >= 500 || cancelled() {
        return false;
    }
    thread::sleep(Duration::from_millis(10));
    !cancelled()
}

#[derive(Deserialize)]
struct Command {
    #[serde(default)]
    operation: String,
    #[serde(default)]
    sql: String,
    #[serde(default)]
    parameters: Vec<Value>,
}

fn execute(database: &mut Database, request: &str) -> Result<serde_json::Value, Error> {
    let command: Command = serde_json::from_str(request)?;
    if command.operation == "exec" {
        let results = database.execute(&command.sql)?;
        return Ok(json!({
            "columns": [], "rows": [],
            "affected": results.last().map_or(0, |result| result.affected),
            "last_insert_rowid": database.last_insert_rowid(),
        }));
    }
    if command.operation == "reset" {
        if database.in_transaction() {
            database.execute_native("ROLLBACK", &[])?;
        }
        database.execute_native("PRAGMA query_only=OFF", &[])?;
        return Ok(json!({}));
    }
    if command.operation == "prepare" {
        let info = database.statement_info(&command.sql)?;
        return Ok(json!({"parameters": info.parameters}));
    }
    let result = database.execute_native(&command.sql, &command.parameters)?;
    Ok(json!({
        "columns": result.columns,
        "rows": result.rows,
        "affected": result.affected,
        "last_insert_rowid": database.last_insert_rowid(),
    }))
}

fn response(result: Result<serde_json::Value, Error>) -> String {
    match result {
        Ok(value) => json!({"result": value}).to_string(),
        Err(error) => json!({"error": error}).to_string(),
    }
}

fn string(value: String) -> *mut c_char {
    CString::new(value)
        .expect("JSON contains no NUL bytes")
        .into_raw()
}

/// Open a database on a dedicated Rust thread; Database contains thread-local Rc values.
///
/// # Safety
/// `path` must be a valid NUL-terminated string and `error` a writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_open(
    path: *const c_char,
    readable: bool,
    error: *mut *mut c_char,
) -> *mut Handle {
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(path) => path.to_owned(),
        Err(e) => {
            unsafe { *error = string(serde_json::to_string(&Error::validation(e)).unwrap()) };
            return std::ptr::null_mut();
        }
    };
    let (sender, receiver) = mpsc::channel::<Request>();
    let (ready, opened) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let worker = thread::spawn(move || {
        CANCELLATION.with(|flag| *flag.borrow_mut() = Some(worker_cancelled));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let database = if path == ":memory:" {
                Database::try_memory()
            } else {
                Database::open(
                    path,
                    if readable {
                        Format::Readable
                    } else {
                        Format::Binary
                    },
                )
            };
            let mut database = match database {
                Ok(database) => {
                    if let Err(error) = database.busy_handler(Some(busy)) {
                        let _ = ready.send(Err(error));
                        return;
                    }
                    let _ = ready.send(Ok(database.interrupt_handle()));
                    database
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            while let Ok((request, reply)) = receiver.recv() {
                let result = catch_unwind(AssertUnwindSafe(|| execute(&mut database, &request)))
                    .unwrap_or_else(|_| Err(Error::internal("BoxDB worker panicked")));
                let _ = reply.send(response(result));
            }
        }));
    });
    match opened
        .recv()
        .unwrap_or_else(|_| Err(Error::internal("BoxDB worker stopped while opening")))
    {
        Ok(interrupt) => Box::into_raw(Box::new(Handle {
            sender: Some(sender),
            worker: Some(worker),
            interrupt,
            cancelled,
        })),
        Err(message) => {
            let _ = worker.join();
            unsafe { *error = string(serde_json::to_string(&message).unwrap()) };
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `handle` must remain live until this call returns. It may be called during execution.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_interrupt(handle: *const Handle) {
    unsafe { &*handle }.cancelled.store(true, Ordering::Relaxed);
    unsafe { &*handle }.interrupt.interrupt();
}

/// # Safety
/// `handle` must be live, with no active operation or interrupt calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_clear_interrupt(handle: *const Handle) {
    unsafe { &*handle }
        .cancelled
        .store(false, Ordering::Relaxed);
}

/// # Safety
/// `handle` must be live, and `request` a valid NUL-terminated JSON string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_call(
    handle: *const Handle,
    request: *const c_char,
) -> *mut c_char {
    let result = (|| {
        let request = unsafe { CStr::from_ptr(request) }
            .to_str()
            .map_err(Error::validation)?;
        let (reply, answer) = mpsc::sync_channel(1);
        unsafe { &*handle }
            .sender
            .as_ref()
            .ok_or_else(|| Error::closed("database is closed"))?
            .send((request.to_owned(), reply))
            .map_err(|_| Error::internal("BoxDB worker stopped"))?;
        answer
            .recv()
            .map_err(|_| Error::internal("BoxDB worker stopped"))
    })();
    string(result.unwrap_or_else(|error| response(Err(error))))
}

/// # Safety
/// `handle` must have been returned by open and must be closed exactly once, with no active calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_close(handle: *mut Handle) {
    let mut handle = unsafe { Box::from_raw(handle) };
    handle.sender.take();
    if let Some(worker) = handle.worker.take() {
        let _ = worker.join();
    }
}

/// # Safety
/// `value` must be a string returned by this library, freed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn boxdb_go_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
