use boxdb::{Database, Format, Type, Value};

#[test]
fn native_decimal_casts_and_operators_keep_decimal_precision() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT typeof(CAST('0.1' AS DECIMAL)), CAST(CAST('9007199254740992.01' AS DECIMAL)+CAST('0.02' AS DECIMAL) AS TEXT), CAST(CAST('0.1' AS DECIMAL)*3 AS TEXT)")
            .unwrap()[0].rows,
        vec![vec![
            Value::Text("decimal".into()),
            Value::Text("9007199254740992.03".into()),
            Value::Text("0.3".into()),
        ]]
    );
}

#[test]
fn decimal_resource_limits_use_normalized_digits_and_scale() {
    let mut db = Database::memory();
    let large = format!("1{}", "0".repeat(4096));
    assert_eq!(
        db.execute("SELECT CAST('1e4096' AS DECIMAL)").unwrap()[0].rows,
        vec![vec![Value::Decimal(large.clone())]]
    );
    assert_eq!(
        Type::Decimal(None)
            .validate(&Value::Decimal(large.clone()))
            .unwrap(),
        Value::Decimal(large)
    );
    let negative = format!("-{}", "9".repeat(4096));
    assert_eq!(
        db.execute_params("SELECT ?1", &[Value::Decimal(negative.clone())])
            .unwrap()[0]
            .rows,
        vec![vec![Value::Decimal(negative)]]
    );
    assert!(db.execute("SELECT CAST('1e4097' AS DECIMAL)").is_err());
    assert!(db.execute("SELECT CAST('1e-4097' AS DECIMAL)").is_err());
}

#[test]
fn exact_decimal_division_checks_the_final_scale_after_exponents_cancel() {
    let mut db = Database::memory();
    let denominator = num_bigint::BigInt::from(2).pow(5000).to_string();
    // 10^4096 / 2^5000 = 5^5000 / 10^904, within the decimal limits.
    assert_eq!(
        db.execute_params(
            "SELECT typeof(CAST('1e4096' AS DECIMAL)/CAST(? AS DECIMAL))",
            &[Value::Text(denominator)]
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Text("decimal".into())]]
    );
}

#[test]
fn decimal_text_and_blob_transports_report_the_size_of_the_returned_representation() {
    use rusqlite::ffi;
    unsafe extern "C" {
        fn sqlite3_column_text16(
            statement: *mut ffi::sqlite3_stmt,
            column: i32,
        ) -> *const std::ffi::c_void;
        fn sqlite3_column_bytes16(statement: *mut ffi::sqlite3_stmt, column: i32) -> i32;
    }
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    struct Statement(*mut ffi::sqlite3_stmt);
    impl Drop for Statement {
        fn drop(&mut self) {
            unsafe {
                ffi::sqlite3_finalize(self.0);
            }
        }
    }
    let mut raw = std::ptr::null_mut();
    // SAFETY: the connection, prepared statement and borrowed buffers stay live.
    unsafe {
        assert_eq!(
            ffi::sqlite3_prepare_v2(
                connection.handle(),
                c"SELECT CAST('123.456' AS DECIMAL)".as_ptr(),
                -1,
                &mut raw,
                std::ptr::null_mut()
            ),
            ffi::SQLITE_OK
        );
        let statement = Statement(raw);
        assert_eq!(ffi::sqlite3_step(statement.0), ffi::SQLITE_ROW);
        let text = ffi::sqlite3_column_text(statement.0, 0);
        assert_eq!(ffi::sqlite3_column_bytes(statement.0, 0), 7);
        assert_eq!(std::slice::from_raw_parts(text, 7), b"123.456");
        let text16 = sqlite3_column_text16(statement.0, 0).cast::<u16>();
        assert_eq!(sqlite3_column_bytes16(statement.0, 0), 14);
        assert_eq!(
            std::slice::from_raw_parts(text16, 7),
            &[49, 50, 51, 46, 52, 53, 54]
        );
        let blob = ffi::sqlite3_column_blob(statement.0, 0).cast::<u8>();
        let length = ffi::sqlite3_column_bytes(statement.0, 0);
        assert_eq!(length, 8);
        assert_eq!(
            libsqlite3_sys::decimal::decode(std::slice::from_raw_parts(blob, length as usize)),
            Some("123.456")
        );
    }
}

#[test]
fn decimal_sum_is_exact_even_when_integer_inputs_overflow_before_promotion() {
    let mut db = Database::memory();
    for values in [
        "(9223372036854775807),(1),(CAST('0' AS DECIMAL))",
        "(CAST('0' AS DECIMAL)),(9223372036854775807),(1)",
    ] {
        assert_eq!(
            db.execute(&format!(
                "WITH vals(v) AS (VALUES {values}) SELECT SUM(v) FROM vals"
            ))
            .unwrap()[0]
                .rows,
            vec![vec![Value::Decimal("9223372036854775808".into())]]
        );
    }
    assert!(
        db.execute("WITH vals(v) AS (VALUES(9223372036854775807),(1)) SELECT SUM(v) FROM vals")
            .is_err()
    );
    assert_eq!(
        db.execute(
            "WITH vals(v) AS (VALUES(CAST('1.25' AS DECIMAL)),(2.0)) SELECT TOTAL(v) FROM vals"
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Real(3.25)]]
    );
}

#[test]
fn decimal_rust_values_and_arrays_have_their_own_serialized_type() {
    let ty = Type::parse("DECIMAL(12,2)").unwrap();
    assert!(Type::parse("NUMERIC(12,2)").is_err());
    assert_eq!(ty, Type::Decimal(Some((12, 2))));
    assert_eq!(serde_json::to_string(&ty).unwrap(), r#"{"Decimal":[12,2]}"#);
    let decimal = Value::Decimal("12.50".into());
    assert_eq!(
        serde_json::from_str::<Value>(r#"{"Decimal":"12.50"}"#).unwrap(),
        decimal
    );
    let bytes = b"\0BDd12".to_vec();
    let mut db = Database::memory();
    db.execute("CREATE TABLE data(v DECIMAL(12,2),history DECIMAL(12,2)[],blobs BYTEA[])")
        .unwrap();
    let values = vec![
        decimal,
        Value::Array(vec![Value::Decimal("1.25".into()), Value::Null]),
        Value::Array(vec![Value::Bytea(bytes)]),
    ];
    db.execute_params("INSERT INTO data VALUES(?,?,?)", &values)
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM data").unwrap()[0].rows,
        vec![values]
    );
}

#[test]
fn decimal_sql_pipelines_preserve_types_constraints_and_range_frames() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE source(v DECIMAL PRIMARY KEY, doubled DECIMAL AS (v*2)); INSERT INTO source(v) VALUES('1'),('2'),('3'); CREATE TABLE copied AS SELECT v,doubled,v+1 AS next FROM source; CREATE TABLE child(v DECIMAL REFERENCES source(v)); INSERT INTO child VALUES('2.00')").unwrap();
    assert_eq!(
        db.execute("SELECT typeof(v),typeof(doubled),typeof(next) FROM copied")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("decimal".into()); 3]; 3]
    );
    assert!(db.execute("INSERT INTO child VALUES('4')").is_err());
    assert_eq!(db.execute("SELECT CAST(SUM(v) OVER (ORDER BY v RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) AS TEXT) FROM source").unwrap()[0].rows,
        vec![vec![Value::Text("1".into())],vec![Value::Text("3".into())],vec![Value::Text("5".into())]]);
    assert_eq!(db.execute("SELECT COUNT(*) FROM (SELECT CAST('2' AS DECIMAL) UNION SELECT 2 UNION SELECT CAST('2.00' AS DECIMAL))").unwrap()[0].rows,vec![vec![Value::Integer(1)]]);
    db.execute("CREATE TEMP TABLE temporary(v DECIMAL); INSERT INTO temporary VALUES('2.5')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT v*2 FROM temporary").unwrap()[0].rows,
        vec![vec![Value::Decimal("5".into())]]
    );
}

#[test]
fn decimal_ctas_keeps_assignment_affinity() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE source(v DECIMAL); INSERT INTO source VALUES('1.5'); CREATE TABLE copy AS SELECT v FROM source; INSERT INTO copy VALUES('2.5')").unwrap();
    assert_eq!(
        db.execute("SELECT typeof(v),v*2 FROM copy ORDER BY v")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Text("decimal".into()), Value::Decimal("3".into())],
            vec![Value::Text("decimal".into()), Value::Decimal("5".into())]
        ]
    );
}

#[test]
fn decimal_columns_can_be_added_to_temp_and_attached_tables() {
    let mut db = Database::memory();
    db.execute("CREATE TEMP TABLE t(id INTEGER); ALTER TABLE temp.t ADD COLUMN amount DECIMAL(5,2); INSERT INTO temp.t VALUES(1,'1.25'); ATTACH ':memory:' AS other; CREATE TABLE other.t(id INTEGER); ALTER TABLE other.t ADD COLUMN amount DECIMAL(5,2); INSERT INTO other.t VALUES(1,'2.50')").unwrap();
    assert_eq!(
        db.execute("SELECT amount*2 FROM temp.t UNION ALL SELECT amount*2 FROM other.t")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Decimal("2.5".into())],
            vec![Value::Decimal("5".into())]
        ]
    );
    assert!(db.execute("INSERT INTO other.t VALUES(2,'1.234')").is_err());
}

#[test]
fn decimal_assignment_to_existing_types_requires_a_lossless_conversion() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(i INTEGER,r REAL,s TEXT,n TEXT); INSERT INTO t VALUES(CAST('12' AS DECIMAL),CAST('0.5' AS DECIMAL),CAST('12.5' AS DECIMAL),CAST('0.1' AS DECIMAL))").unwrap();
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap()[0].rows,
        vec![vec![
            Value::Integer(12),
            Value::Real(0.5),
            Value::Text("12.5".into()),
            Value::Text("0.1".into())
        ]]
    );
    for sql in [
        "INSERT INTO t(i) VALUES(CAST('1.5' AS DECIMAL))",
        "INSERT INTO t(r) VALUES(CAST('0.1' AS DECIMAL))",
        "INSERT INTO t(i) VALUES(CAST('9223372036854775808' AS DECIMAL))",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
}

#[test]
fn explicit_decimal_rounding_and_division_round_once_to_the_requested_scale() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT decimal_round('123.456',2,'half_even'), decimal_round('2',2,'half_up'), decimal_div('1','3',2,'half_even'), decimal_div('-1.245','1',2,'half_up'), decimal_div('1.245','1',2,'half_even')").unwrap()[0].rows,
        vec![vec![Value::Decimal("123.46".into()),Value::Decimal("2.00".into()),Value::Decimal("0.33".into()),Value::Decimal("-1.25".into()),Value::Decimal("1.24".into())]]);
    let above_tie = format!("1.245{}1", "0".repeat(150));
    assert_eq!(
        db.execute_params(
            "SELECT decimal_div(?1,'1',2,'half_even')",
            &[Value::Text(above_tie)]
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Decimal("1.25".into())]]
    );
    for sql in [
        "SELECT decimal_div('1','0',2,'half_even')",
        "SELECT decimal_round('1',-1,'half_even')",
        "SELECT decimal_round('1',2,'unknown')",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
}

#[test]
fn decimal_rounding_propagates_null_in_every_argument() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT decimal_round(NULL,2,'half_even'),decimal_round('1',NULL,'half_even'),decimal_round('1',2,NULL),decimal_div(NULL,'3',2,'half_even'),decimal_div('1',NULL,2,'half_even'),decimal_div('1','3',NULL,'half_even'),decimal_div('1','3',2,NULL)").unwrap()[0].rows,
        vec![vec![Value::Null;7]]);
}

#[test]
fn bytea_values_cannot_be_mistaken_for_decimal_envelopes() {
    let mut db = Database::memory();
    let bytes = b"\0BDd12".to_vec();
    assert_eq!(
        db.execute_params("SELECT typeof(?1),?1", &[Value::Bytea(bytes.clone())])
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("blob".into()),
            Value::Bytea(bytes.clone())
        ]]
    );
    db.execute("CREATE TABLE blobs(v BYTEA); INSERT INTO blobs VALUES(X'004244643132'),(CAST(char(0)||'BDd12' AS BLOB))").unwrap();
    assert_eq!(
        db.execute("SELECT v FROM blobs").unwrap()[0].rows,
        vec![vec![Value::Bytea(bytes.clone())], vec![Value::Bytea(bytes)]]
    );
    let snapshot = db.export(Format::Binary).unwrap();
    let mut restored = Database::from_snapshot(&snapshot).unwrap();
    assert_eq!(
        restored
            .execute("SELECT typeof(v),hex(v) FROM blobs")
            .unwrap()[0]
            .rows,
        vec![
            vec![
                Value::Text("blob".into()),
                Value::Text("004244643132".into())
            ];
            2
        ]
    );
}

#[test]
fn blob_functions_and_raw_bindings_preserve_reserved_bytes() {
    let mut db = Database::memory();
    let bytes = b"\0BDd12".to_vec();
    assert_eq!(
        db.execute(
            "SELECT typeof(unhex('004244643132')),unhex('004244643132'),hex(unhex('004244643132'))"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("blob".into()),
            Value::Bytea(bytes.clone()),
            Value::Text("004244643132".into())
        ]]
    );
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT typeof(?1),hex(?1)", [bytes], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            )))
            .unwrap(),
        ("blob".into(), "004244643132".into())
    );
}

#[test]
fn blob_text_casts_and_utf16_decimal_operands_use_logical_payloads() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST(X'004244643132' AS TEXT)||'!',hex(CAST(CAST(X'004244643132' AS TEXT) AS BLOB))").unwrap()[0].rows,
        vec![vec![Value::Text("\0BDd12!".into()),Value::Text("004244643132".into())]]);
    for encoding in ["UTF-16le", "UTF-16be"] {
        let mut db = Database::memory();
        db.execute(&format!(
            "PRAGMA encoding='{encoding}'; CREATE TABLE t(v DECIMAL); INSERT INTO t VALUES('1.5')"
        ))
        .unwrap();
        assert_eq!(
            db.execute("SELECT v+'2',CAST(v AS TEXT),v||' EUR' FROM t")
                .unwrap()[0]
                .rows,
            vec![vec![
                Value::Decimal("3.5".into()),
                Value::Text("1.5".into()),
                Value::Text("1.5 EUR".into())
            ]]
        );
    }
}

#[test]
fn decimal_truth_casts_and_numeric_functions_do_not_read_blob_headers_as_zero() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CASE WHEN CAST('1e-4096' AS DECIMAL) THEN 1 ELSE 0 END, NOT CAST('2' AS DECIMAL), CAST(abs(CAST('-2' AS DECIMAL)) AS TEXT), CAST(round(CAST('1.235' AS DECIMAL),2) AS TEXT), CAST(CAST('12.5' AS DECIMAL) AS INTEGER), CAST(CAST('12.5' AS DECIMAL) AS REAL)").unwrap()[0].rows,
        vec![vec![Value::Integer(1),Value::Integer(0),Value::Text("2".into()),Value::Text("1.24".into()),Value::Integer(12),Value::Real(12.5)]]);
}

#[test]
fn decimal_conversions_are_explicit_and_text_keeps_all_digits() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST(0.1 AS DECIMAL), CAST('12.5' AS DECIMAL)||' EUR', CAST(CAST('0.1' AS DECIMAL) AS TEXT)").unwrap()[0].rows,
        vec![vec![Value::Decimal("0.1000000000000000055511151231257827021181583404541015625".into()),Value::Text("12.5 EUR".into()),Value::Text("0.1".into())]]);
    assert_eq!(
        Type::Decimal(None).cast(Value::Real(0.1)).unwrap(),
        Value::Decimal("0.1000000000000000055511151231257827021181583404541015625".into())
    );
}

#[test]
fn decimal_aggregates_groups_and_windows_preserve_numeric_values() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE amounts(v DECIMAL); INSERT INTO amounts VALUES('1'),('2'),('3')")
        .unwrap();
    assert_eq!(db.execute("SELECT CAST(SUM(v) AS TEXT), CAST(AVG(v) AS TEXT), CAST(MIN(v) AS TEXT), CAST(MAX(v) AS TEXT) FROM amounts").unwrap()[0].rows,
        vec![vec![Value::Text("6".into()),Value::Text("2".into()),Value::Text("1".into()),Value::Text("3".into())]]);
    assert_eq!(db.execute("SELECT CAST(SUM(v) OVER (ORDER BY rowid ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS TEXT), CAST(AVG(v) OVER (ORDER BY rowid ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS TEXT) FROM amounts").unwrap()[0].rows,
        vec![vec![Value::Text("1".into()),Value::Text("1".into())],vec![Value::Text("3".into()),Value::Text("1.5".into())],vec![Value::Text("5".into()),Value::Text("2.5".into())]]);
    assert_eq!(db.execute("WITH vals(v) AS (VALUES(CAST('2.00' AS DECIMAL)),(2),(CAST('10' AS DECIMAL))) SELECT COUNT(*) FROM vals GROUP BY v ORDER BY v").unwrap()[0].rows,
        vec![vec![Value::Integer(2)],vec![Value::Integer(1)]]);
}

#[test]
fn decimal_division_and_remainder_are_exact_and_reject_lossy_results() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST(CAST('10' AS DECIMAL)/4 AS TEXT), CAST(CAST('-5.5' AS DECIMAL)%2 AS TEXT), typeof(-CAST('2' AS DECIMAL))").unwrap()[0].rows,
        vec![vec![Value::Text("2.5".into()), Value::Text("-1.5".into()), Value::Text("decimal".into())]]);
    for sql in [
        "SELECT CAST('1' AS DECIMAL)/3",
        "SELECT CAST('1' AS DECIMAL)/0",
        "SELECT CAST('1' AS DECIMAL)+0.1",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert_eq!(
        db.execute("SELECT CAST('1' AS DECIMAL)+NULL").unwrap()[0].rows,
        vec![vec![Value::Null]]
    );
}

#[test]
fn decimal_index_search_and_uniqueness_agree_with_full_scans() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE mixed(v ANY); INSERT INTO mixed VALUES(CAST('-10' AS DECIMAL)),(CAST('-2' AS DECIMAL)),(CAST('2.00' AS DECIMAL)),(CAST('10' AS DECIMAL)); CREATE UNIQUE INDEX numbers ON mixed(v)").unwrap();
    for predicate in [
        "v=2",
        "v<2",
        "v>=2",
        "v BETWEEN -2 AND 10",
        "v IN (-2,2)",
        "v<'0'",
        "v>'0'",
    ] {
        let query = |index: &str| {
            format!("SELECT CAST(v AS TEXT) FROM mixed {index} WHERE {predicate} ORDER BY v")
        };
        assert_eq!(
            db.execute(&query("INDEXED BY numbers")).unwrap()[0].rows,
            db.execute(&query("NOT INDEXED")).unwrap()[0].rows,
            "{predicate}"
        );
    }
    assert!(db.execute("INSERT INTO mixed VALUES(2)").is_err());
    assert!(
        db.execute("INSERT INTO mixed VALUES(CAST('2.0' AS DECIMAL))")
            .is_err()
    );
    assert_eq!(
        db.execute("PRAGMA integrity_check").unwrap()[0].rows,
        vec![vec![Value::Text("ok".into())]]
    );
}

#[test]
fn decimal_comparisons_promote_numeric_text_exactly_without_changing_operands() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE native(v DECIMAL); CREATE TABLE legacy(v TEXT); INSERT INTO native VALUES('0.1'),('9007199254740992.01'); INSERT INTO legacy VALUES('0.1'),('9007199254740992.01')").unwrap();
    assert_eq!(
        db.execute("SELECT COUNT(*) FROM native JOIN legacy ON native.v=legacy.v")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        db.execute("SELECT COUNT(*) FROM native WHERE v IN (SELECT v FROM legacy)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        db.execute(
            "SELECT '0.1'=CAST('0.1' AS DECIMAL),typeof('0.1'),2=CAST('2' AS DECIMAL),typeof(2)"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Integer(1),
            Value::Text("text".into()),
            Value::Integer(1),
            Value::Text("integer".into())
        ]]
    );
}

#[test]
fn text_column_comparisons_to_computed_decimals_agree_with_index_searches() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE legacy(v TEXT); INSERT INTO legacy VALUES('0.1'); CREATE INDEX text_index ON legacy(v)").unwrap();
    for plan in ["INDEXED BY text_index", "NOT INDEXED"] {
        assert_eq!(
            db.execute(&format!(
                "SELECT v FROM legacy {plan} WHERE v=decimal_round('0.1',1,'half_even')"
            ))
            .unwrap()[0]
                .rows,
            vec![vec![Value::Text("0.1".into())]]
        );
    }
}

#[test]
fn native_decimal_columns_and_computed_values_survive_reopening_and_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    for format in [Format::Binary, Format::Readable] {
        let path = directory.path().join(format!("{format:?}"));
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE prices(amount DECIMAL(20,2), other DECIMAL(20,2)); INSERT INTO prices VALUES('12.50','12.50')").unwrap();
        let snapshot = db.export(format).unwrap();
        drop(db);
        for mut restored in [
            Database::open(&path, format).unwrap(),
            Database::from_snapshot(&snapshot).unwrap(),
        ] {
            let row = &restored
                .execute(
                    "SELECT amount,other,amount+CAST('0.25' AS DECIMAL),typeof(amount) FROM prices",
                )
                .unwrap()[0]
                .rows;
            assert_eq!(
                serde_json::to_value(row).unwrap(),
                serde_json::json!([[{"Decimal":"12.50"},{"Decimal":"12.50"},{"Decimal":"12.75"},{"Text":"decimal"}]])
            );
            assert!(
                restored
                    .execute("INSERT INTO prices VALUES('1.234','1.23')")
                    .is_err()
            );
        }
    }
}

#[test]
fn native_decimal_metadata_requires_native_decimal_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mismatched.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE prices(amount TEXT) STRICT; CREATE TABLE __boxdb_columns(table_name TEXT NOT NULL,column_name TEXT NOT NULL,logical_type TEXT NOT NULL,table_sql TEXT NOT NULL,version INTEGER NOT NULL CHECK(version=1),PRIMARY KEY(table_name,column_name)) STRICT;").unwrap();
        connection.execute("INSERT INTO __boxdb_columns SELECT 'prices','amount','{\"Decimal\":null}',sql,1 FROM sqlite_schema WHERE name='prices'",[]).unwrap();
    }
    assert!(Database::open(&path, Format::Binary).is_err());
}

#[test]
fn native_decimal_ordering_and_equality_are_numeric_across_materialized_values() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("WITH numbers(v) AS MATERIALIZED (VALUES(CAST('10' AS DECIMAL)),(CAST('-2' AS DECIMAL)),(CAST('-10' AS DECIMAL)),(2),(CAST('2.00' AS DECIMAL)),(1.5)) SELECT CAST(v AS TEXT) FROM numbers ORDER BY v")
            .unwrap()[0].rows,
        vec![
            vec![Value::Text("-10".into())],
            vec![Value::Text("-2".into())],
            vec![Value::Text("1.5".into())],
            vec![Value::Text("2".into())],
            vec![Value::Text("2".into())],
            vec![Value::Text("10".into())],
        ]
    );
    assert_eq!(
        db.execute(
            "SELECT CAST('2.00' AS DECIMAL)=2, CAST('0.1' AS DECIMAL)<0.1, CAST('-0' AS DECIMAL)=0"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(1)
        ]]
    );
}

#[test]
fn decimal_sorting_with_large_temporary_records_keeps_standard_binary_format() {
    let mut db = Database::memory();
    db.execute("PRAGMA temp_store=FILE; PRAGMA cache_size=-64; CREATE TABLE data(v DECIMAL,pad TEXT); WITH RECURSIVE seq(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM seq WHERE i<3999) INSERT INTO data SELECT printf('%d.25',3999-i),printf('%2048s','x') FROM seq").unwrap();
    let results = db.execute("SELECT v,pad FROM data ORDER BY v,pad").unwrap();
    let rows = &results[0].rows;
    assert_eq!(rows.len(), 4000);
    for (index, expected) in [
        (0, "0.25"),
        (1, "1.25"),
        (10, "10.25"),
        (100, "100.25"),
        (3999, "3999.25"),
    ] {
        assert_eq!(rows[index][0], Value::Decimal(expected.into()));
    }
    let bytes = db.export(Format::Binary).unwrap();
    assert_eq!(&bytes[..16], b"SQLite format 3\0");
    let mut restored = Database::from_snapshot(&bytes).unwrap();
    assert_eq!(
        restored
            .execute("SELECT v FROM data ORDER BY v DESC LIMIT 2")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Decimal("3999.25".into())],
            vec![Value::Decimal("3998.25".into())]
        ]
    );
}
