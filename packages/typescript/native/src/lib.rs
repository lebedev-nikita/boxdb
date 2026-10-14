use boxdb::{Database, Format, Value};
use napi::Env;
use napi::bindgen_prelude::{BigInt, Buffer, Either5, JsObjectValue, Null, ToNapiValue};
use napi_derive::napi;

type Cell = Either5<Null, f64, BigInt, String, Buffer>;

fn failure(error: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(error.to_string())
}

fn database_failure(env: Env, error: boxdb::Error) -> napi::Error {
    let result = (|| -> napi::Result<napi::Error> {
        let mut object = env.create_error(failure(&error))?;
        object.set_named_property("code", error.code())?;
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

fn input(value: Cell) -> napi::Result<Value> {
    Ok(match value {
        Either5::A(_) => Value::Null,
        Either5::B(value) => Value::Real(value),
        Either5::C(value) => {
            let (value, lossless) = value.get_i64();
            if !lossless {
                return Err(failure("bigint is outside the SQLite signed 64-bit range"));
            }
            Value::Integer(value)
        }
        Either5::D(value) => Value::Text(value),
        Either5::E(value) => Value::Bytea(value.to_vec()),
    })
}

fn output(value: Value) -> napi::Result<Cell> {
    Ok(match value {
        Value::Null => Either5::A(Null),
        Value::Integer(value) => Either5::C(value.into()),
        Value::Real(value) => Either5::B(value),
        Value::Text(value) => Either5::D(value),
        Value::Bytea(value) => Either5::E(value.into()),
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

/// A synchronous, owning connection. It is never moved to a worker thread.
#[napi]
pub struct NativeDatabase {
    database: Option<Database>,
}

impl NativeDatabase {
    fn database(&mut self, env: Env) -> napi::Result<&mut Database> {
        self.database
            .as_mut()
            .ok_or_else(|| database_failure(env, boxdb::Error::closed("database is closed")))
    }
}

#[napi]
impl NativeDatabase {
    #[napi(constructor)]
    pub fn new(env: Env, path: String, format: String) -> napi::Result<Self> {
        let format = match format.as_str() {
            "binary" => Format::Binary,
            "readable" => Format::Readable,
            _ => return Err(failure("format must be binary or readable")),
        };
        let database = if path == ":memory:" {
            Database::try_memory().map_err(|error| database_failure(env, error))?
        } else {
            Database::open(path, format).map_err(|error| database_failure(env, error))?
        };
        Ok(Self {
            database: Some(database),
        })
    }

    #[napi]
    pub fn prepare(&mut self, env: Env, sql: String) -> napi::Result<NativeInfo> {
        let info = self
            .database(env)?
            .statement_info(&sql)
            .map_err(|error| database_failure(env, error))?;
        Ok(NativeInfo {
            columns: info.columns,
            parameters: info.parameters,
            readonly: info.readonly,
        })
    }

    #[napi]
    pub fn execute(
        &mut self,
        env: Env,
        sql: String,
        parameters: Vec<Cell>,
    ) -> napi::Result<NativeResult> {
        let parameters = parameters
            .into_iter()
            .map(input)
            .collect::<napi::Result<Vec<_>>>()?;
        let database = self.database(env)?;
        let result = database
            .execute_native(&sql, &parameters)
            .map_err(|error| database_failure(env, error))?;
        Ok(NativeResult {
            columns: result.columns,
            rows: result
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(output).collect())
                .collect::<napi::Result<_>>()?,
            changes: (result.affected as u64).into(),
            last_insert_rowid: database.last_insert_rowid().into(),
        })
    }

    #[napi]
    pub fn exec(&mut self, env: Env, sql: String) -> napi::Result<()> {
        self.database(env)?
            .execute(&sql)
            .map_err(|error| database_failure(env, error))?;
        Ok(())
    }

    #[napi(getter)]
    pub fn in_transaction(&mut self, env: Env) -> napi::Result<bool> {
        Ok(self.database(env)?.in_transaction())
    }

    #[napi]
    pub fn close(&mut self) {
        self.database.take();
    }
}
