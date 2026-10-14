use super::{Cell, NativeInfo, NativeResult, database_failure_with_state, failure, input, output};
use boxdb::{Database, Format, InterruptHandle, OpenOptions, QueryResult, StatementInfo, Value};
use napi::{Env, Task, bindgen_prelude::AsyncTask};
use napi_derive::napi;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

thread_local! {
    static CANCELLATION: RefCell<Option<(Arc<AtomicBool>, Instant, Duration)>> = const { RefCell::new(None) };
}

fn cancelled() -> bool {
    CANCELLATION.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|(flag, _, _)| flag.load(Ordering::Relaxed))
    })
}

fn busy(attempt: i32) -> bool {
    if attempt == 0 {
        CANCELLATION.with(|state| {
            if let Some((_, start, _)) = state.borrow_mut().as_mut() {
                *start = Instant::now();
            }
        });
    }
    let waiting = || {
        CANCELLATION.with(|state| {
            state
                .borrow()
                .as_ref()
                .is_some_and(|(flag, start, timeout)| {
                    !flag.load(Ordering::Relaxed) && start.elapsed() < *timeout
                })
        })
    };
    if !waiting() {
        return false;
    }
    thread::sleep(Duration::from_millis(10));
    waiting()
}

#[derive(Default)]
struct Control {
    requests: HashMap<u32, Arc<AtomicBool>>,
    active: Option<(u32, InterruptHandle)>,
}

pub enum Reply {
    Empty,
    Info(StatementInfo),
    Rows(QueryResult, i64),
}

struct Request {
    id: u32,
    cancelled: Arc<AtomicBool>,
    operation: String,
    sql: String,
    parameters: Vec<Value>,
    read_only: bool,
    reply: mpsc::Sender<WorkerReply>,
}

pub struct WorkerReply {
    result: boxdb::Result<Reply>,
    in_transaction: bool,
}

#[napi(object)]
pub struct NativeResponse {
    pub in_transaction: bool,
    pub info: Option<NativeInfo>,
    pub result: Option<NativeResult>,
}

pub struct ResponseTask {
    receiver: mpsc::Receiver<WorkerReply>,
}

impl Task for ResponseTask {
    type Output = WorkerReply;
    type JsValue = NativeResponse;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.receiver
            .recv()
            .map_err(|_| failure("BoxDB worker stopped"))
    }

    fn resolve(&mut self, env: Env, reply: Self::Output) -> napi::Result<Self::JsValue> {
        let in_transaction = reply.in_transaction;
        let reply = reply
            .result
            .map_err(|error| database_failure_with_state(env, error, Some(in_transaction)))?;
        let mut response = NativeResponse {
            info: None,
            result: None,
            in_transaction,
        };
        match reply {
            Reply::Empty => {}
            Reply::Info(info) => {
                response.info = Some(NativeInfo {
                    columns: info.columns,
                    parameters: info.parameters,
                    readonly: info.readonly,
                })
            }
            Reply::Rows(result, rowid) => {
                response.result = Some(NativeResult {
                    columns: result.columns,
                    rows: result
                        .rows
                        .into_iter()
                        .map(|row| row.into_iter().map(output).collect())
                        .collect::<napi::Result<_>>()?,
                    changes: (result.affected as u64).into(),
                    last_insert_rowid: rowid.into(),
                })
            }
        }
        Ok(response)
    }
}

/// The non-Send Database is created, used and dropped on one dedicated thread.
#[napi]
pub struct NativeDatabase {
    sender: mpsc::Sender<Request>,
    control: Arc<Mutex<Control>>,
}

#[napi]
impl NativeDatabase {
    #[napi(constructor)]
    pub fn new(
        path: String,
        format: String,
        read_only: bool,
        busy_timeout_ms: u32,
    ) -> napi::Result<Self> {
        let format = match format.as_str() {
            "binary" => Format::Binary,
            "readable" => Format::Readable,
            _ => return Err(failure("format must be binary or readable")),
        };
        let (sender, receiver) = mpsc::channel::<Request>();
        let control = Arc::new(Mutex::new(Control::default()));
        let worker_control = control.clone();
        thread::Builder::new()
            .name("boxdb-typescript".into())
            .spawn(move || {
                let mut database = None;
                let mut query_only_restore = None;
                while let Ok(request) = receiver.recv() {
                    CANCELLATION.with(|state| {
                        *state.borrow_mut() = Some((
                            request.cancelled.clone(),
                            Instant::now(),
                            Duration::from_millis(u64::from(busy_timeout_ms)),
                        ))
                    });
                    let result = (|| -> boxdb::Result<Reply> {
                        if request.cancelled.load(Ordering::Relaxed) {
                            return Err(boxdb::Error::new("operation cancelled".into()));
                        }
                        if request.operation == "open" {
                            database = Some(if path == ":memory:" {
                                Database::memory_with_options(OpenOptions {
                                    read_only,
                                    busy_handler: Some(busy),
                                    progress_handler: Some(cancelled),
                                })?
                            } else {
                                Database::open_with_options(
                                    &path,
                                    format,
                                    OpenOptions {
                                        read_only,
                                        busy_handler: Some(busy),
                                        progress_handler: Some(cancelled),
                                    },
                                )?
                            });
                            return Ok(Reply::Empty);
                        }
                        if request.operation == "close" {
                            database.take();
                            return Ok(Reply::Empty);
                        }
                        let db = database
                            .as_mut()
                            .ok_or_else(|| boxdb::Error::closed("database is closed"))?;
                        if matches!(request.operation.as_str(), "prepare" | "execute" | "exec") {
                            Database::validate_query_sql(&request.sql)?;
                        }
                        let query_only = read_only || request.read_only;
                        if query_only {
                            let previous = db.query_only(true)?;
                            query_only_restore.get_or_insert(previous);
                        } else if let Some(previous) = query_only_restore {
                            db.query_only(previous)?;
                            query_only_restore = None;
                        }
                        db.busy_handler(Some(busy))?;
                        db.progress_handler(1000, Some(cancelled))?;
                        worker_control.lock().unwrap().active =
                            Some((request.id, db.interrupt_handle()));
                        if request.cancelled.load(Ordering::Relaxed) {
                            return Err(boxdb::Error::new("operation cancelled".into()));
                        }
                        match request.operation.as_str() {
                            "rollback" => {
                                if db.in_transaction() {
                                    db.execute_native("ROLLBACK", &[])?;
                                }
                                Ok(Reply::Empty)
                            }
                            "prepare" => Ok(Reply::Info(db.statement_info(&request.sql)?)),
                            "exec" | "control" => {
                                db.execute_with_hook(&request.sql, |db| {
                                    if cancelled() {
                                        return Err(boxdb::Error::new(
                                            "operation cancelled".into(),
                                        ));
                                    }
                                    if query_only {
                                        db.query_only(true)?;
                                    }
                                    db.busy_handler(Some(busy))
                                })?;
                                Ok(Reply::Empty)
                            }
                            "execute" => {
                                let result =
                                    db.execute_native(&request.sql, &request.parameters)?;
                                Ok(Reply::Rows(result, db.last_insert_rowid()))
                            }
                            _ => Err(boxdb::Error::internal("unknown operation")),
                        }
                    })();
                    let mut control = worker_control.lock().unwrap();
                    control.active = None;
                    control.requests.remove(&request.id);
                    drop(control);
                    CANCELLATION.with(|state| *state.borrow_mut() = None);
                    let _ = request.reply.send(WorkerReply {
                        result,
                        in_transaction: database.as_ref().is_some_and(Database::in_transaction),
                    });
                    if request.operation == "close" {
                        break;
                    }
                }
            })
            .map_err(failure)?;
        Ok(Self { sender, control })
    }

    #[napi]
    pub fn call(
        &self,
        id: u32,
        operation: String,
        sql: String,
        parameters: Vec<Cell>,
        read_only: bool,
    ) -> napi::Result<AsyncTask<ResponseTask>> {
        let parameters = parameters
            .into_iter()
            .map(input)
            .collect::<napi::Result<Vec<_>>>()?;
        let (reply, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.control
            .lock()
            .unwrap()
            .requests
            .insert(id, cancelled.clone());
        if self
            .sender
            .send(Request {
                id,
                cancelled,
                operation,
                sql,
                parameters,
                read_only,
                reply,
            })
            .is_err()
        {
            self.control.lock().unwrap().requests.remove(&id);
            return Err(failure("BoxDB worker stopped"));
        }
        Ok(AsyncTask::new(ResponseTask { receiver }))
    }

    #[napi]
    pub fn cancel(&self, id: u32) {
        let control = self.control.lock().unwrap();
        if let Some(flag) = control.requests.get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some((active, interrupt)) = &control.active
            && *active == id
        {
            interrupt.interrupt();
        }
    }
}
