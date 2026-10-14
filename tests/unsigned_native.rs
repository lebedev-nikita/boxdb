use boxdb::{Database, Format, Type, Value};

#[test]
fn uint64_preserves_full_range_in_parameters_literals_and_snapshots() {
    assert_eq!(Type::parse("UINT64").unwrap(), Type::Uint64);
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n UINT64, a UINT64[])").unwrap();
    db.execute_params(
        "INSERT INTO t VALUES(?,?)",
        &[
            Value::UnsignedInteger(u64::MAX),
            Value::Array(vec![Value::UnsignedInteger(u64::MAX), Value::Null]),
        ],
    )
    .unwrap();
    db.execute("INSERT INTO t(n) VALUES(9223372036854775808),(0),(18446744073709551615)")
        .unwrap();
    let rows = db.execute("SELECT n FROM t ORDER BY rowid").unwrap()[0]
        .rows
        .clone();
    assert_eq!(
        rows,
        vec![
            vec![Value::UnsignedInteger(u64::MAX)],
            vec![Value::UnsignedInteger(1 << 63)],
            vec![Value::UnsignedInteger(0)],
            vec![Value::UnsignedInteger(u64::MAX)]
        ]
    );
    for format in [Format::Binary, Format::Readable] {
        let snapshot = db.export(format).unwrap();
        let mut restored = Database::from_snapshot(&snapshot).unwrap();
        assert_eq!(
            restored.execute("SELECT n FROM t ORDER BY rowid").unwrap()[0].rows,
            rows
        );
        assert_eq!(
            restored.execute("SELECT a FROM t WHERE rowid=1").unwrap()[0].rows,
            vec![vec![Value::Array(vec![
                Value::UnsignedInteger(u64::MAX),
                Value::Null
            ])]]
        );
    }
}

#[test]
fn unsigned_comparisons_are_numeric_with_and_without_indexes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n UINT64); INSERT INTO t VALUES(18446744073709551615),(9223372036854775808),(9223372036854775807),(0),(9007199254740993),(9007199254740992)").unwrap();
    for indexed in [false, true] {
        if indexed {
            db.execute("CREATE UNIQUE INDEX ix ON t(n)").unwrap();
        }
        assert_eq!(
            db.execute("SELECT n FROM t WHERE n>9223372036854775807 ORDER BY n")
                .unwrap()[0]
                .rows,
            vec![
                vec![Value::UnsignedInteger(1 << 63)],
                vec![Value::UnsignedInteger(u64::MAX)]
            ]
        );
        assert_eq!(
            db.execute("SELECT n FROM t WHERE n=9007199254740993")
                .unwrap()[0]
                .rows,
            vec![vec![Value::UnsignedInteger(9007199254740993)]]
        );
        assert_eq!(db.execute("SELECT CAST(18446744073709551615 AS UINT64)>-1, CAST(9007199254740993 AS UINT64)>9007199254740992.0, CAST(0 AS UINT64)=0").unwrap()[0].rows, vec![vec![Value::Integer(1), Value::Integer(1), Value::Integer(1)]]);
    }
    assert_eq!(
        db.execute("PRAGMA integrity_check").unwrap()[0].rows,
        vec![vec![Value::Text("ok".into())]]
    );
}

#[test]
fn all_unsigned_widths_have_checked_operations_and_logical_shifts() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(a UINT8,b UINT16,c UINT32,d UINT64); INSERT INTO t VALUES(254,65534,4294967294,18446744073709551614)").unwrap();
    assert_eq!(
        db.execute("SELECT a+1,b+1,c+1,d+1,typeof(a+1),typeof(b+1),typeof(c+1),typeof(d+1) FROM t")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::UnsignedInteger(255),
            Value::UnsignedInteger(65535),
            Value::UnsignedInteger(4294967295),
            Value::UnsignedInteger(u64::MAX),
            Value::Text("uint8".into()),
            Value::Text("uint16".into()),
            Value::Text("uint32".into()),
            Value::Text("uint64".into())
        ]]
    );
    assert_eq!(db.execute("SELECT CAST(255 AS UINT8)>>1, ~CAST(0 AS UINT8), CAST(1 AS UINT64)<<63, CAST(65535 AS UINT16)&255, CAST(255 AS UINT8)+CAST(1 AS UINT16), CAST(7 AS UINT8)/2, CAST(7 AS UINT8)%2").unwrap()[0].rows, vec![vec![Value::UnsignedInteger(127),Value::UnsignedInteger(255),Value::UnsignedInteger(1 << 63),Value::UnsignedInteger(255),Value::UnsignedInteger(256),Value::UnsignedInteger(3),Value::UnsignedInteger(1)]]);
    for sql in [
        "SELECT a+2 FROM t",
        "SELECT b+2 FROM t",
        "SELECT c+2 FROM t",
        "SELECT d+2 FROM t",
        "SELECT CAST(0 AS UINT8)-1",
        "SELECT CAST(1 AS UINT64)/0",
        "SELECT CAST(-1 AS UINT64)",
        "SELECT CAST(1.5 AS UINT8)",
        "SELECT CAST(18446744073709551616 AS UINT64)",
        "SELECT d+0.5 FROM t",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert_eq!(db.execute("WITH v(x) AS MATERIALIZED (SELECT a FROM t UNION ALL SELECT CAST(1 AS UINT8)) SELECT typeof(x),x+1 FROM v ORDER BY x").unwrap()[0].rows, vec![vec![Value::Text("uint8".into()),Value::UnsignedInteger(2)],vec![Value::Text("uint8".into()),Value::UnsignedInteger(255)]]);
}

fn varint(bytes: &[u8], at: &mut usize) -> u64 {
    let mut n = 0;
    for i in 0..9 {
        let b = bytes[*at];
        *at += 1;
        if i == 8 {
            return (n << 8) | u64::from(b);
        }
        n = (n << 7) | u64::from(b & 127);
        if b < 128 {
            return n;
        }
    }
    unreachable!()
}
#[test]
fn typed_columns_use_standard_integer_records_for_small_uints() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(a UINT8,b UINT16,c UINT32,d UINT64); INSERT INTO t VALUES(255,65535,4294967295,18446744073709551615)").unwrap();
    let root = match db
        .execute("SELECT rootpage FROM sqlite_schema WHERE name='t'")
        .unwrap()[0]
        .rows[0][0]
    {
        Value::Integer(n) => n as usize,
        _ => panic!(),
    };
    let bytes = db.export(Format::Binary).unwrap();
    let page_size = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    let page = &bytes[(root - 1) * page_size..root * page_size];
    assert_eq!(page[0], 13);
    let mut at = usize::from(u16::from_be_bytes([page[8], page[9]]));
    varint(page, &mut at);
    varint(page, &mut at);
    let payload = &page[at..];
    let mut header = 0;
    assert_eq!(varint(payload, &mut header), 5);
    assert_eq!(
        (0..4)
            .map(|_| varint(payload, &mut header))
            .collect::<Vec<_>>(),
        vec![2, 3, 5, 30]
    );
    assert_eq!(
        &payload[header..header + 11],
        &[0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255]
    );
    assert_eq!(
        &payload[header + 11..header + 20],
        &[b'Q', 255, 255, 255, 255, 255, 255, 255, 255]
    );
    assert_eq!(
        db.execute("SELECT typeof(a),typeof(b),typeof(c),typeof(d),a,b,c,d FROM t")
            .unwrap()[0]
            .rows[0],
        vec![
            Value::Text("uint8".into()),
            Value::Text("uint16".into()),
            Value::Text("uint32".into()),
            Value::Text("uint64".into()),
            Value::UnsignedInteger(255),
            Value::UnsignedInteger(65535),
            Value::UnsignedInteger(4294967295),
            Value::UnsignedInteger(u64::MAX)
        ]
    );
}

#[test]
fn hybrid_values_work_in_indexes_generated_columns_any_and_aggregates() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n UINT8 PRIMARY KEY,w UINT64,g UINT16 AS (n+CAST(1 AS UINT16)) STORED) WITHOUT ROWID; INSERT INTO t(n,w) VALUES(1,18446744073709551615),(254,9223372036854775808); CREATE INDEX ix ON t(g); CREATE TABLE any_values(v ANY); INSERT INTO any_values SELECT n FROM t; CREATE TABLE copied AS SELECT n,w FROM t").unwrap();
    assert_eq!(
        db.execute("SELECT typeof(n),n+1,typeof(g),g FROM t WHERE g=255")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("uint8".into()),
            Value::UnsignedInteger(255),
            Value::Text("uint16".into()),
            Value::UnsignedInteger(255)
        ]]
    );
    assert_eq!(
        db.execute("SELECT typeof(v),v+1 FROM any_values ORDER BY v")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Text("uint8".into()), Value::UnsignedInteger(2)],
            vec![Value::Text("uint8".into()), Value::UnsignedInteger(255)]
        ]
    );
    assert_eq!(
        db.execute("SELECT sum(n),avg(n),sum(w),avg(w) FROM t")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::UnsignedInteger(255),
            Value::Decimal("127.5".into()),
            Value::Decimal("27670116110564327423".into()),
            Value::Decimal("13835058055282163711.5".into())
        ]]
    );
    assert_eq!(db.execute("SELECT CAST(CAST(255 AS UINT8) AS INTEGER), CAST(CAST(18446744073709551615 AS UINT64) AS DECIMAL), CAST(CAST(18446744073709551615 AS UINT64) AS REAL), NOT CAST(0 AS UINT8),NOT CAST(1 AS UINT8)").unwrap()[0].rows,vec![vec![Value::Integer(255),Value::Decimal("18446744073709551615".into()),Value::Real(18446744073709551615u64 as f64),Value::Integer(1),Value::Integer(0)]]);
    assert!(
        db.execute("SELECT CAST(CAST(18446744073709551615 AS UINT64) AS INTEGER)")
            .is_err()
    );
    assert_eq!(
        db.execute("PRAGMA integrity_check").unwrap()[0].rows,
        vec![vec![Value::Text("ok".into())]]
    );
}

#[test]
fn indexed_small_uints_preserve_width_and_compare_outside_the_column_range() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n UINT8); INSERT INTO t VALUES(0),(1),(127),(128),(255); CREATE UNIQUE INDEX ix ON t(n)").unwrap();
    for suffix in ["", "INDEXED BY ix", "NOT INDEXED"] {
        assert_eq!(db.execute(&format!("SELECT n,typeof(n),n+CAST(1 AS UINT16) FROM t {suffix} WHERE n>=128 AND n<300 ORDER BY n")).unwrap()[0].rows,vec![vec![Value::UnsignedInteger(128),Value::Text("uint8".into()),Value::UnsignedInteger(129)],vec![Value::UnsignedInteger(255),Value::Text("uint8".into()),Value::UnsignedInteger(256)]]);
        assert_eq!(
            db.execute(&format!(
                "SELECT n FROM t {suffix} WHERE n IN(1,255,300) ORDER BY n"
            ))
            .unwrap()[0]
                .rows,
            vec![
                vec![Value::UnsignedInteger(1)],
                vec![Value::UnsignedInteger(255)]
            ]
        );
    }
    assert_eq!(
        db.execute("SELECT typeof(n),n FROM t WHERE n=1").unwrap()[0].rows,
        vec![vec![Value::Text("uint8".into()), Value::UnsignedInteger(1)]]
    );
    assert_eq!(db.execute("SELECT CAST(255 AS UINT8)<<1,CAST(1 AS UINT8)<<8,CAST(255 AS UINT8)>>8,CAST(1 AS UINT8)+CAST('0.5' AS DECIMAL)").unwrap()[0].rows,vec![vec![Value::UnsignedInteger(254),Value::UnsignedInteger(0),Value::UnsignedInteger(0),Value::Decimal("1.5".into())]]);
}

#[test]
fn unsigned_parameters_and_casts_obey_numeric_column_ranges() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE signed(n INTEGER); CREATE TABLE t(n UINT64 DEFAULT 18446744073709551615); CREATE TEMP TABLE scratch(n UINT16); ATTACH ':memory:' AS aux; CREATE TABLE aux.t(n UINT8)").unwrap();
    db.execute_params(
        "INSERT INTO signed VALUES(?)",
        &[Value::UnsignedInteger(42)],
    )
    .unwrap();
    assert!(
        db.execute_params(
            "INSERT INTO signed VALUES(?)",
            &[Value::UnsignedInteger(u64::MAX)]
        )
        .is_err()
    );
    db.execute("INSERT INTO t DEFAULT VALUES; INSERT INTO scratch VALUES(65535); INSERT INTO aux.t VALUES(255)").unwrap();
    assert_eq!(
        db.execute("SELECT n FROM signed").unwrap()[0].rows,
        vec![vec![Value::Integer(42)]]
    );
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::UnsignedInteger(u64::MAX)]]
    );
    assert_eq!(
        db.execute("SELECT typeof(n),n FROM scratch").unwrap()[0].rows,
        vec![vec![
            Value::Text("uint16".into()),
            Value::UnsignedInteger(65535)
        ]]
    );
    assert_eq!(
        db.execute("SELECT typeof(n),n FROM aux.t").unwrap()[0].rows,
        vec![vec![
            Value::Text("uint8".into()),
            Value::UnsignedInteger(255)
        ]]
    );
    assert_eq!(
        Type::Uint64
            .cast(Value::Decimal("18446744073709551615.0".into()))
            .unwrap(),
        Value::UnsignedInteger(u64::MAX)
    );
    assert_eq!(
        Type::Decimal(None)
            .cast(Value::UnsignedInteger(u64::MAX))
            .unwrap(),
        Value::Decimal("18446744073709551615".into())
    );
}

#[test]
fn automatic_indexes_and_bloom_filters_keep_mixed_numeric_equality() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE u(v UINT64); CREATE TABLE r(v REAL); WITH RECURSIVE seq(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM seq WHERE n<199) INSERT INTO u SELECT 9223372036854775808+n FROM seq; INSERT INTO r VALUES(9223372036854775808.0)").unwrap();
    for enabled in [false, true] {
        db.execute(if enabled {
            "PRAGMA automatic_index=ON"
        } else {
            "PRAGMA automatic_index=OFF"
        })
        .unwrap();
        assert_eq!(
            db.execute("SELECT u.v FROM r CROSS JOIN u ON u.v=r.v")
                .unwrap()[0]
                .rows,
            vec![vec![Value::UnsignedInteger(1 << 63)]]
        );
        assert_eq!(
            db.execute("SELECT u.v FROM u CROSS JOIN r ON u.v=r.v")
                .unwrap()[0]
                .rows,
            vec![vec![Value::UnsignedInteger(1 << 63)]]
        );
    }
    assert_eq!(db.execute("SELECT CAST(18446744073709551615 AS UINT64)<1e999, CAST(0 AS UINT8)>-1e999, CAST(1 AS UINT8)=1e999").unwrap()[0].rows,vec![vec![Value::Integer(1),Value::Integer(1),Value::Integer(0)]]);
}

#[test]
fn expression_indexes_keep_unsigned_result_types() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1),(254); CREATE INDEX ix ON t(CAST(n AS UINT8))").unwrap();
    assert_eq!(db.execute("SELECT CAST(n AS UINT8),typeof(CAST(n AS UINT8)),CAST(n AS UINT8)+1 FROM t INDEXED BY ix WHERE CAST(n AS UINT8)=254").unwrap()[0].rows,vec![vec![Value::UnsignedInteger(254),Value::Text("uint8".into()),Value::UnsignedInteger(255)]]);
    db.execute("REINDEX ix; VACUUM").unwrap();
    assert_eq!(
        db.execute("PRAGMA integrity_check").unwrap()[0].rows,
        vec![vec![Value::Text("ok".into())]]
    );
}

#[test]
fn explicit_unsigned_casts_accept_only_exact_integral_real_values() {
    assert_eq!(
        Type::Uint64
            .cast(Value::Real(9223372036854775808.0))
            .unwrap(),
        Value::UnsignedInteger(1 << 63)
    );
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT CAST(255.0 AS UINT8),CAST(9223372036854775808.0 AS UINT64),CAST(CAST('255.00' AS DECIMAL) AS UINT8)").unwrap()[0].rows,vec![vec![Value::UnsignedInteger(255),Value::UnsignedInteger(1 << 63),Value::UnsignedInteger(255)]]);
    for sql in [
        "SELECT CAST(1.5 AS UINT8)",
        "SELECT CAST(18446744073709551615.0 AS UINT64)",
        "SELECT CAST(1e999 AS UINT64)",
        "SELECT CAST(-1.0 AS UINT64)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
}

#[test]
fn real_indexes_do_not_round_unsigned_search_keys() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE r(v REAL); INSERT INTO r VALUES(9007199254740992.0),(9223372036854775808.0)",
    )
    .unwrap();
    for indexed in [false, true] {
        if indexed {
            db.execute("CREATE INDEX ix ON r(v)").unwrap();
        }
        for n in [9007199254740993, 9223372036854775809] {
            assert!(
                db.execute_params("SELECT v FROM r WHERE v=?", &[Value::UnsignedInteger(n)])
                    .unwrap()[0]
                    .rows
                    .is_empty(),
                "{n}, indexed={indexed}"
            );
        }
        assert_eq!(
            db.execute_params(
                "SELECT v FROM r WHERE v=?",
                &[Value::UnsignedInteger(1 << 63)]
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Real(9223372036854775808.0)]]
        );
    }
}
