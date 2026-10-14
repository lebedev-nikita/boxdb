use boxdb::Value;
use napi::Env;
use napi::bindgen_prelude::{BigInt, Buffer, Either5, JsObjectValue, Null, ToNapiValue};
use napi_derive::napi;

mod asynchronous;

type Cell = Either5<Null, f64, BigInt, String, Buffer>;

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
        Value::Text(value)
        | Value::Decimal(value)
        | Value::Uuid(value)
        | Value::Instant(value)
        | Value::Date(value)
        | Value::DateTime(value)
        | Value::Interval(value) => Either5::D(value),
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
