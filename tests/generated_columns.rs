use boxdb::{Database, Format, Value};

#[test]
fn short_and_full_declarations_preserve_the_requested_storage_mode() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE names (
            name TEXT,
            short_default TEXT AS (lower(name)),
            short_virtual TEXT AS (lower(name)) VIRTUAL,
            short_stored TEXT AS (lower(name)) STORED,
            full_default TEXT GENERATED ALWAYS AS (lower(name)),
            full_virtual TEXT GENERATED ALWAYS AS (lower(name)) VIRTUAL,
            full_stored TEXT GENERATED ALWAYS AS (lower(name)) STORED
        )",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT name,hidden FROM pragma_table_xinfo('names') ORDER BY cid")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Text("name".into()), Value::Integer(0)],
            vec![Value::Text("short_default".into()), Value::Integer(2)],
            vec![Value::Text("short_virtual".into()), Value::Integer(2)],
            vec![Value::Text("short_stored".into()), Value::Integer(3)],
            vec![Value::Text("full_default".into()), Value::Integer(2)],
            vec![Value::Text("full_virtual".into()), Value::Integer(2)],
            vec![Value::Text("full_stored".into()), Value::Integer(3)],
        ]
    );
    db.execute("INSERT INTO names VALUES('Ada'); UPDATE names SET name='Grace'")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM names").unwrap()[0].rows,
        vec![vec![
            Value::Text("Grace".into()),
            Value::Text("grace".into()),
            Value::Text("grace".into()),
            Value::Text("grace".into()),
            Value::Text("grace".into()),
            Value::Text("grace".into()),
            Value::Text("grace".into()),
        ]]
    );
}

#[test]
fn reopened_files_preserve_generated_values_after_commit_and_rollback() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("generated");
        let mut db = Database::open(&path, format).unwrap();
        db.execute(
            "CREATE TABLE numbers(n INT16,doubled INT16 AS (n*2),label TEXT AS (printf('n=%d',doubled)) STORED);
            INSERT INTO numbers(n) VALUES(3);
            BEGIN; UPDATE numbers SET n=4; ROLLBACK",
        )
        .unwrap();
        drop(db);
        let mut reopened = Database::open(&path, format).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM numbers").unwrap()[0].rows,
            vec![vec![
                Value::Integer(3),
                Value::Integer(6),
                Value::Text("n=6".into())
            ]],
            "{format:?}"
        );
        reopened
            .execute("BEGIN; UPDATE numbers SET n=5; COMMIT")
            .unwrap();
        drop(reopened);
        let mut reopened = Database::open(&path, format).unwrap();
        assert_eq!(
            reopened.execute("SELECT * FROM numbers").unwrap()[0].rows,
            vec![vec![
                Value::Integer(5),
                Value::Integer(10),
                Value::Text("n=10".into())
            ]],
            "{format:?}"
        );
        assert_eq!(
            reopened
                .execute("UPDATE numbers SET n=6 RETURNING doubled,label")
                .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(12), Value::Text("n=12".into())]],
            "{format:?}"
        );
        assert!(reopened.execute("UPDATE numbers SET n=20000").is_err());
        drop(reopened);
        assert_eq!(
            Database::open(&path, format)
                .unwrap()
                .execute("SELECT * FROM numbers")
                .unwrap()[0]
                .rows,
            vec![vec![
                Value::Integer(6),
                Value::Integer(12),
                Value::Text("n=12".into())
            ]],
            "{format:?}"
        );
    }
}

#[test]
fn snapshots_restore_typed_generated_columns_from_their_source_values() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE derived (
            id INT64 PRIMARY KEY,
            enabled_source BOOLEAN,
            amount_source TEXT,
            enabled BOOLEAN AS (enabled_source),
            code UINT8 AS (enabled+7) STORED,
            amount DECIMAL(6,2) AS (amount_source) STORED
        );
        INSERT INTO derived(id,enabled_source,amount_source) VALUES(1,1,'12.50')",
    )
    .unwrap();
    for format in [Format::Binary, Format::Readable] {
        let bytes = db.export(format).unwrap();
        if format == Format::Readable {
            let documents: Vec<serde_json::Value> =
                serde_saphyr::from_slice_multiple(&bytes).unwrap();
            let row = documents
                .iter()
                .find(|doc| doc["kind"] == "row" && doc["table"] == "derived")
                .unwrap();
            assert_eq!(
                row["values"],
                serde_json::json!({
                    "id": 1,
                    "enabled_source": 1,
                    "amount_source": "12.50"
                })
            );
        }
        let mut restored = Database::from_snapshot(&bytes).unwrap();
        assert_eq!(
            restored
                .execute("SELECT enabled,code,amount FROM derived")
                .unwrap()[0]
                .rows,
            vec![vec![
                Value::Boolean(true),
                Value::UnsignedInteger(8),
                Value::Decimal("12.50".into())
            ]],
            "{format:?}"
        );
        assert!(
            restored
                .execute("UPDATE derived SET amount_source='12.345'")
                .is_err()
        );
        restored
            .execute("UPDATE derived SET enabled_source=0,amount_source='20.00'")
            .unwrap();
        assert_eq!(
            restored
                .execute("SELECT enabled,code,amount FROM derived")
                .unwrap()[0]
                .rows,
            vec![vec![
                Value::Boolean(false),
                Value::UnsignedInteger(7),
                Value::Decimal("20.00".into())
            ]],
            "{format:?}"
        );
    }
}

#[test]
fn failed_generated_column_additions_leave_the_existing_schema_usable() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE amounts(input TEXT); INSERT INTO amounts VALUES('12.50'),('1.234')")
        .unwrap();
    for definition in [
        "amount DECIMAL(6,2) AS (input)",
        "amount DECIMAL(6,2) AS (input) STORED",
    ] {
        assert!(
            db.execute(&format!("ALTER TABLE amounts ADD COLUMN {definition}"))
                .is_err()
        );
        let result = db.execute("SELECT * FROM amounts ORDER BY input").unwrap();
        assert_eq!(result[0].columns, vec!["input"]);
        assert_eq!(
            result[0].rows,
            vec![
                vec![Value::Text("1.234".into())],
                vec![Value::Text("12.50".into())]
            ]
        );
    }
    db.execute("UPDATE amounts SET input='1.23' WHERE input='1.234'; ALTER TABLE amounts ADD COLUMN amount DECIMAL(6,2) AS (input)")
        .unwrap();
    assert_eq!(
        db.execute("SELECT amount FROM amounts ORDER BY input")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Decimal("1.23".into())],
            vec![Value::Decimal("12.50".into())]
        ]
    );
}

#[test]
fn adding_and_renaming_generated_columns_preserves_values_and_logical_types() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE amounts(id INT64 PRIMARY KEY,input TEXT);
        INSERT INTO amounts VALUES(1,'12.50'),(2,NULL);
        ALTER TABLE amounts ADD COLUMN amount DECIMAL(6,2) AS (input)",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT amount FROM amounts ORDER BY id")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Decimal("12.50".into())], vec![Value::Null]]
    );
    db.execute(
        "ALTER TABLE amounts RENAME COLUMN input TO source;
        ALTER TABLE amounts RENAME COLUMN amount TO computed;
        ALTER TABLE amounts RENAME TO prices;
        UPDATE prices SET source='25.75' WHERE id=1",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT computed AS amount FROM prices ORDER BY id")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Decimal("25.75".into())], vec![Value::Null]]
    );
    assert!(
        db.execute("UPDATE prices SET source='25.751' WHERE id=1")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT source,computed FROM prices WHERE id=1")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("25.75".into()),
            Value::Decimal("25.75".into())
        ]]
    );
}

#[test]
fn unsupported_generated_definitions_fail_without_creating_a_table() {
    for mode in ["VIRTUAL", "STORED"] {
        for definition in [
            format!("n INT64,value INT64 AS (n) {mode} DEFAULT 1"),
            format!("n INT64,value INT64 AS (n) {mode} PRIMARY KEY"),
            format!("n INT64,value INT64 AS (random()) {mode}"),
            format!("n INT64,value INT64 AS (sum(n)) {mode}"),
            format!("n INT64,value INT64 AS ((SELECT 1)) {mode}"),
            format!("n INT64,value INT64 AS (row_number() OVER ()) {mode}"),
            format!("n INT64,value INT64 AS (rowid) {mode}"),
            format!("value INT64 AS (1) {mode}"),
        ] {
            let mut db = Database::memory();
            assert!(
                db.execute(&format!("CREATE TABLE invalid({definition})"))
                    .is_err(),
                "{definition}"
            );
            assert!(
                db.execute("SELECT name FROM sqlite_schema WHERE name='invalid'")
                    .unwrap()[0]
                    .rows
                    .is_empty(),
                "{definition}"
            );
        }
    }
}

#[test]
fn indexed_generated_columns_enforce_constraints_and_follow_updates() {
    for mode in ["VIRTUAL", "STORED"] {
        let mut db = Database::memory();
        db.execute(&format!(
            "CREATE TABLE users (
                id INT64 PRIMARY KEY,
                name TEXT,
                normalized TEXT AS (lower(trim(name))) {mode} NOT NULL CHECK(length(normalized)>0)
            );
            CREATE UNIQUE INDEX users_normalized ON users(normalized);
            INSERT INTO users(id,name) VALUES(1,' Ada '),(2,'Grace')"
        ))
        .unwrap();
        for sql in [
            "INSERT INTO users(id,name) VALUES(3,NULL)",
            "INSERT INTO users(id,name) VALUES(3,'   ')",
            "INSERT INTO users(id,name) VALUES(3,'Linus'),(4,'ADA')",
            "UPDATE users SET name=' GRACE ' WHERE id=1",
        ] {
            assert!(db.execute(sql).is_err(), "{mode}: {sql}");
        }
        assert_eq!(
            db.execute("SELECT id,normalized FROM users ORDER BY normalized")
                .unwrap()[0]
                .rows,
            vec![
                vec![Value::Integer(1), Value::Text("ada".into())],
                vec![Value::Integer(2), Value::Text("grace".into())],
            ],
            "{mode}"
        );
        db.execute("UPDATE users SET name=' Linus ' WHERE id=1; INSERT INTO users(id,name) VALUES(3,'ADA')")
            .unwrap();
        assert_eq!(
            db.execute("SELECT id FROM users WHERE normalized='linus'")
                .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(1)]],
            "{mode}"
        );
        assert_eq!(
            db.execute("SELECT id FROM users WHERE normalized='ada'")
                .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(3)]],
            "{mode}"
        );
    }
}

#[test]
fn generated_columns_apply_logical_types_to_reads_and_atomic_writes() {
    let cases = [
        (
            "BOOLEAN",
            Value::Integer(1),
            Value::Boolean(true),
            Value::Integer(2),
        ),
        (
            "UINT8",
            Value::Integer(255),
            Value::UnsignedInteger(255),
            Value::Integer(256),
        ),
        (
            "VARCHAR(3)",
            Value::Text("猫🙂".into()),
            Value::Text("猫🙂".into()),
            Value::Text("long".into()),
        ),
        (
            "DECIMAL(6,2)",
            Value::Text("12.50".into()),
            Value::Decimal("12.50".into()),
            Value::Text("12.345".into()),
        ),
        (
            "UUID",
            Value::Text("550e8400-e29b-41d4-a716-446655440000".into()),
            Value::Uuid("550e8400-e29b-41d4-a716-446655440000".into()),
            Value::Text("invalid".into()),
        ),
        (
            "JSON",
            Value::Text("{\"x\":1}".into()),
            Value::Json(serde_json::json!({"x": 1})),
            Value::Text("{".into()),
        ),
        (
            "UINT8[]",
            Value::Text("[1,null,255]".into()),
            Value::Array(vec![
                Value::UnsignedInteger(1),
                Value::Null,
                Value::UnsignedInteger(255),
            ]),
            Value::Text("[256]".into()),
        ),
    ];
    for mode in ["VIRTUAL", "STORED"] {
        for (ty, input, expected, invalid) in &cases {
            let mut db = Database::memory();
            db.execute(&format!(
                "CREATE TABLE typed(id INT64 PRIMARY KEY,input ANY,value {ty} AS (input) {mode});
                CREATE VIEW projection AS SELECT id,value AS computed FROM typed"
            ))
            .unwrap();
            db.execute_params(
                "INSERT INTO typed(id,input) VALUES(1,?)",
                std::slice::from_ref(input),
            )
            .unwrap();
            assert_eq!(
                db.execute("SELECT computed AS result FROM projection")
                    .unwrap()[0]
                    .rows,
                vec![vec![expected.clone()]],
                "{mode}: {ty}"
            );
            assert!(
                db.execute_params(
                    "INSERT INTO typed(id,input) VALUES(2,?),(3,?)",
                    &[input.clone(), invalid.clone()]
                )
                .is_err(),
                "{mode}: {ty}"
            );
            assert!(
                db.execute_params(
                    "UPDATE typed SET input=? WHERE id=1",
                    std::slice::from_ref(invalid)
                )
                .is_err(),
                "{mode}: {ty}"
            );
            assert_eq!(
                db.execute("SELECT id,value FROM typed").unwrap()[0].rows,
                vec![vec![Value::Integer(1), expected.clone()]],
                "{mode}: {ty}"
            );
            db.execute("INSERT INTO typed(id,input) VALUES(2,NULL)")
                .unwrap();
            assert_eq!(
                db.execute("SELECT value FROM typed WHERE id=2").unwrap()[0].rows,
                vec![vec![Value::Null]],
                "{mode}: {ty}"
            );
        }
    }
}

#[test]
fn generated_columns_reject_explicit_insert_update_and_upsert_values() {
    for mode in ["VIRTUAL", "STORED"] {
        let mut db = Database::memory();
        db.execute(&format!(
            "CREATE TABLE numbers(id INT64 PRIMARY KEY,n INT64,doubled INT64 AS (n*2) {mode});
            INSERT INTO numbers(id,n) VALUES(1,3)"
        ))
        .unwrap();
        for sql in [
            "INSERT INTO numbers(id,n,doubled) VALUES(2,4,8)",
            "INSERT INTO numbers(id,n,doubled) VALUES(2,4,NULL)",
            "UPDATE numbers SET n=4,doubled=8 WHERE id=1",
            "INSERT INTO numbers(id,n) VALUES(1,4) ON CONFLICT(id) DO UPDATE SET doubled=8",
        ] {
            assert!(db.execute(sql).is_err(), "{mode}: {sql}");
            assert_eq!(
                db.execute("SELECT * FROM numbers").unwrap()[0].rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(3),
                    Value::Integer(6)
                ]],
                "{mode}: {sql}"
            );
        }
        db.execute(
            "INSERT INTO numbers(id,n) VALUES(1,4) ON CONFLICT(id) DO UPDATE SET n=excluded.n",
        )
        .unwrap();
        assert_eq!(
            db.execute("SELECT doubled FROM numbers").unwrap()[0].rows,
            vec![vec![Value::Integer(8)]],
            "{mode}"
        );
    }
}

#[test]
fn generated_dependencies_follow_updates_nulls_and_transaction_rollback() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE lines (
            id INT64 PRIMARY KEY,
            quantity INT16,
            unit_cents INT64,
            fee INT64,
            total INT64 AS (subtotal + fee) STORED,
            subtotal INT64 AS (quantity * unit_cents)
        );
        INSERT INTO lines(id,quantity,unit_cents,fee)
        VALUES(1,3,1250,100),(2,NULL,500,50),(3,2,2000,200)",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT id,subtotal,total FROM lines ORDER BY id")
            .unwrap()[0]
            .rows,
        vec![
            vec![
                Value::Integer(1),
                Value::Integer(3750),
                Value::Integer(3850)
            ],
            vec![Value::Integer(2), Value::Null, Value::Null],
            vec![
                Value::Integer(3),
                Value::Integer(4000),
                Value::Integer(4200)
            ],
        ]
    );
    db.execute("BEGIN; UPDATE lines SET quantity=4 WHERE id=1")
        .unwrap();
    assert_eq!(
        db.execute("SELECT id,subtotal,total FROM lines WHERE total>4000 ORDER BY total DESC")
            .unwrap()[0]
            .rows,
        vec![
            vec![
                Value::Integer(1),
                Value::Integer(5000),
                Value::Integer(5100)
            ],
            vec![
                Value::Integer(3),
                Value::Integer(4000),
                Value::Integer(4200)
            ],
        ]
    );
    db.execute("ROLLBACK").unwrap();
    assert_eq!(
        db.execute("SELECT subtotal,total FROM lines WHERE id=1")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(3750), Value::Integer(3850)]]
    );
    db.execute("UPDATE lines SET quantity=1 WHERE id=2")
        .unwrap();
    assert_eq!(
        db.execute("SELECT subtotal,total FROM lines WHERE id=2")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(500), Value::Integer(550)]]
    );
}
