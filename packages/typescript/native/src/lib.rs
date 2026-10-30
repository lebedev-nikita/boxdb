use boxdb::Value;
use napi::Env;
use napi::bindgen_prelude::{BigInt, Buffer, Either6, JsObjectValue, Null, ToNapiValue};
use napi_derive::napi;
use std::sync::Arc;

mod asynchronous;

#[napi(object)]
pub struct NativeMigrationFile {
    pub filename: String,
    pub sql: String,
}

#[napi]
pub struct NativeMigrationSet {
    migrations: Arc<boxdb::migrations::MigrationSet>,
}

#[napi]
impl NativeMigrationSet {
    #[napi(constructor)]
    pub fn new(env: Env, files: Vec<NativeMigrationFile>) -> napi::Result<Self> {
        let migrations =
            boxdb::migrations::MigrationSet::from_files(files.into_iter().map(|file| {
                boxdb::migrations::MigrationFile {
                    filename: file.filename,
                    sql: file.sql,
                }
            }))
            .map_err(|error| database_failure_with_state(env, error, None))?;
        Ok(Self {
            migrations: Arc::new(migrations),
        })
    }
}

#[napi(object)]
pub struct NativeMigrationInfo {
    pub version: String,
    pub name: String,
}

#[napi(object)]
pub struct NativeMigrationStatus {
    pub version: String,
    pub name: String,
    pub state: String,
}

/// A CHANGESET value, distinguished from ordinary BLOB bytes.
#[napi(object)]
pub struct NativeChangeset {
    pub changeset: Buffer,
}

type Cell = Either6<Null, f64, BigInt, String, Buffer, NativeChangeset>;

fn failure(error: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(error.to_string())
}

fn database_failure_with_state(
    env: Env,
    error: boxdb::Error,
    in_transaction: Option<bool>,
) -> napi::Error {
    let result = (|| -> napi::Result<napi::Error> {
        let mut object = env.create_error(failure(&error))?;
        object.set_named_property("code", error.code())?;
        object.set_named_property("details", serde_json::to_string(&error).map_err(failure)?)?;
        if let Some(in_transaction) = in_transaction {
            object.set_named_property("inTransaction", in_transaction)?;
        }
        if let Some(code) = error.result_code() {
            object.set_named_property("resultCode", code)?;
        }
        if let Some(code) = error.extended_code() {
            object.set_named_property("extendedCode", code)?;
        }
        Ok(napi::Error::from(object.into_unknown(&env)?))
    })();
    result.unwrap_or_else(|error| error)
}

pub struct MaintenanceTask {
    operation: String,
    source: String,
    destination: Option<String>,
    mode: String,
}

impl napi::Task for MaintenanceTask {
    type Output = boxdb::Result<String>;
    type JsValue = String;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        Ok((|| {
            let value = match self.operation.as_str() {
                "check" => serde_json::to_string(&boxdb::Database::check_file(
                    &self.source,
                    match self.mode.as_str() {
                        "quick" => boxdb::CheckMode::Quick,
                        "full" => boxdb::CheckMode::Full,
                        _ => return Err(boxdb::Error::new("mode must be quick or full".into())),
                    },
                )?),
                "repair" => serde_json::to_string(&boxdb::Database::repair_file(
                    &self.source,
                    self.destination
                        .as_ref()
                        .ok_or_else(|| boxdb::Error::new("missing destination".into()))?,
                )?),
                "recover" => serde_json::to_string(&boxdb::Database::recover_file(
                    &self.source,
                    self.destination
                        .as_ref()
                        .ok_or_else(|| boxdb::Error::new("missing destination".into()))?,
                    match self.mode.as_str() {
                        "binary" => boxdb::Format::Binary,
                        "readable" => boxdb::Format::Readable,
                        _ => {
                            return Err(boxdb::Error::new(
                                "format must be binary or readable".into(),
                            ));
                        }
                    },
                )?),
                _ => return Err(boxdb::Error::new("unknown maintenance operation".into())),
            };
            Ok(value?)
        })())
    }
    fn resolve(&mut self, env: Env, output: Self::Output) -> napi::Result<String> {
        output.map_err(|error| database_failure_with_state(env, error, None))
    }
}

#[napi]
pub fn maintain_file(
    operation: String,
    source: String,
    destination: Option<String>,
    mode: String,
) -> napi::bindgen_prelude::AsyncTask<MaintenanceTask> {
    napi::bindgen_prelude::AsyncTask::new(MaintenanceTask {
        operation,
        source,
        destination,
        mode,
    })
}

fn input(value: Cell) -> napi::Result<Value> {
    Ok(match value {
        Either6::A(_) => Value::Null,
        Either6::B(value) => Value::Real(value),
        Either6::C(value) => {
            let (signed, lossless) = value.get_i64();
            if lossless {
                Value::Integer(signed)
            } else {
                let (negative, unsigned, lossless) = value.get_u64();
                if negative || !lossless {
                    return Err(failure("bigint is outside the BoxDB 64-bit range"));
                }
                Value::UnsignedInteger(unsigned)
            }
        }
        Either6::D(value) => Value::Text(value),
        Either6::E(value) => Value::Bytea(value.to_vec()),
        Either6::F(value) => Value::Changeset(value.changeset.to_vec()),
    })
}

fn output(value: Value) -> napi::Result<Cell> {
    Ok(match value {
        Value::Null => Either6::A(Null),
        Value::Integer(value) => Either6::C(value.into()),
        Value::UnsignedInteger(value) => Either6::C(value.into()),
        Value::Real(value) => Either6::B(value),
        Value::Text(value)
        | Value::Decimal(value)
        | Value::Uuid(value)
        | Value::Instant(value)
        | Value::Date(value)
        | Value::Time(value)
        | Value::DateTime(value)
        | Value::Interval(value) => Either6::D(value),
        Value::Bytea(value) => Either6::E(value.into()),
        Value::Changeset(value) => Either6::F(NativeChangeset {
            changeset: value.into(),
        }),
        _ => return Err(failure("expected a SQLite storage value")),
    })
}

#[napi(object)]
pub struct NativeInfo {
    pub columns: Vec<String>,
    pub parameters: Vec<Option<String>>,
    pub readonly: bool,
}

#[napi(object)]
pub struct NativeResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub changes: BigInt,
    pub last_insert_rowid: BigInt,
}
