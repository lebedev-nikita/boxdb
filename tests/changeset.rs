use boxdb::Value;
use boxdb::changeset::{self, Change, Operation};

// Hand-encoded per SQLite's documented changeset format: table "t" with two
// columns (the first is the key) and one UPDATE of the second column.
fn update_bytes() -> Vec<u8> {
    let mut bytes = vec![0x54, 0x02, 0x01, 0x00, b't', 0x00, 0x17, 0x00];
    bytes.extend([0x01, 0, 0, 0, 0, 0, 0, 0, 1]);
    bytes.extend([0x03, 0x01, b'a']);
    bytes.push(0x00);
    bytes.extend([0x03, 0x01, b'b']);
    bytes
}

#[test]
fn decodes_sqlite_changeset_records_into_typed_changes() {
    assert_eq!(
        changeset::decode(&update_bytes()).unwrap(),
        vec![Change {
            table: "t".into(),
            primary_key: vec![true, false],
            operation: Operation::Update,
            indirect: false,
            old: Some(vec![Some(Value::Integer(1)), Some(Value::Text("a".into()))]),
            new: Some(vec![None, Some(Value::Text("b".into()))]),
        }]
    );
    let mut truncated = update_bytes();
    truncated.pop();
    assert!(changeset::decode(&truncated).is_err());
    assert!(changeset::decode(b"garbage").is_err());
    assert_eq!(changeset::decode(&[]).unwrap(), vec![]);
}

#[test]
fn inversion_swaps_records_and_keeps_primary_keys_in_the_old_record() {
    let inverted = changeset::invert(&update_bytes()).unwrap();
    let mut expected = vec![0x54, 0x02, 0x01, 0x00, b't', 0x00, 0x17, 0x00];
    expected.extend([0x01, 0, 0, 0, 0, 0, 0, 0, 1]);
    expected.extend([0x03, 0x01, b'b']);
    expected.push(0x00);
    expected.extend([0x03, 0x01, b'a']);
    assert_eq!(inverted, expected);
    let mut insert = vec![0x54, 0x01, 0x01, b'u', 0x00, 0x12, 0x00];
    insert.extend([0x01, 0, 0, 0, 0, 0, 0, 0, 7]);
    let mut delete = insert.clone();
    delete[5] = 0x09;
    assert_eq!(changeset::invert(&insert).unwrap(), delete);
    assert_eq!(changeset::invert(&delete).unwrap(), insert);
}

#[test]
fn concatenation_merges_changes_to_the_same_row() {
    let mut insert = vec![0x54, 0x02, 0x01, 0x00, b't', 0x00, 0x12, 0x00];
    insert.extend([0x01, 0, 0, 0, 0, 0, 0, 0, 1]);
    insert.extend([0x03, 0x01, b'a']);
    let combined = changeset::concat(&insert, &update_bytes()).unwrap();
    assert_eq!(
        changeset::decode(&combined).unwrap(),
        vec![Change {
            table: "t".into(),
            primary_key: vec![true, false],
            operation: Operation::Insert,
            indirect: false,
            old: None,
            new: Some(vec![Some(Value::Integer(1)), Some(Value::Text("b".into()))]),
        }]
    );
    let mut delete = insert.clone();
    delete[6] = 0x09;
    assert_eq!(
        changeset::concat(&insert, &delete).unwrap(),
        Vec::<u8>::new()
    );
    // Deleting and re-inserting identical values changes nothing.
    assert_eq!(
        changeset::concat(&delete, &insert).unwrap(),
        Vec::<u8>::new()
    );
    let mut reinsert = insert.clone();
    *reinsert.last_mut().unwrap() = b'c';
    assert_eq!(
        changeset::decode(&changeset::concat(&delete, &reinsert).unwrap()).unwrap(),
        vec![Change {
            table: "t".into(),
            primary_key: vec![true, false],
            operation: Operation::Update,
            indirect: false,
            old: Some(vec![Some(Value::Integer(1)), Some(Value::Text("a".into()))]),
            new: Some(vec![None, Some(Value::Text("c".into()))]),
        }]
    );
}

#[test]
fn changeset_columns_validate_bytes_and_return_typed_values() {
    let mut db = boxdb::Database::memory();
    db.execute("CREATE TABLE history(id INT64 PRIMARY KEY, changes CHANGESET NOT NULL)")
        .unwrap();
    db.execute_params(
        "INSERT INTO history(changes) VALUES(?)",
        &[Value::Changeset(update_bytes())],
    )
    .unwrap();
    db.execute_params(
        "INSERT INTO history(changes) VALUES(?)",
        &[Value::Bytea(update_bytes())],
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT changes FROM history").unwrap()[0].rows,
        vec![
            vec![Value::Changeset(update_bytes())],
            vec![Value::Changeset(update_bytes())]
        ]
    );
    let rejected = db
        .execute_params(
            "INSERT INTO history(changes) VALUES(?)",
            &[Value::Bytea(b"garbage".to_vec())],
        )
        .unwrap_err();
    assert!(
        rejected.to_string().contains("invalid changeset"),
        "{rejected}"
    );
    assert!(
        db.execute("INSERT INTO history(changes) VALUES('text')")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT typeof(changes), length(changes) FROM history LIMIT 1")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Text("blob".into()),
            Value::Integer(update_bytes().len() as i64)
        ]]
    );
    assert_eq!(
        boxdb::Type::parse("CHANGESET").unwrap(),
        boxdb::Type::Changeset
    );
}

fn recorded(db: &mut boxdb::Database) -> Vec<Change> {
    match &db.execute("SELECT boxdb_changeset()").unwrap()[0].rows[0][0] {
        Value::Bytea(bytes) => changeset::decode(bytes).unwrap(),
        Value::Null => vec![],
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn recording_a_transaction_captures_original_and_current_rows_with_native_cells() {
    let mut db = boxdb::Database::memory();
    db.execute(
        "CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT, budget DECIMAL(12,2), due INSTANT, tags TEXT[]);
         CREATE TABLE untracked(note TEXT);
         INSERT INTO tasks VALUES(1,'a','1.50','2024-01-01T00:00:00Z','[\"x\"]'),(2,'b',NULL,NULL,NULL),(3,'c',NULL,NULL,NULL)",
    )
    .unwrap();
    assert!(db.execute("SELECT boxdb_record(1)").is_err());
    assert!(db.execute("SELECT boxdb_changeset()").is_err());
    db.execute(
        "BEGIN; SELECT boxdb_record(1);
         UPDATE tasks SET title='A', budget='2.25' WHERE id=1;
         DELETE FROM tasks WHERE id=2;
         INSERT INTO tasks(id,title,due) VALUES(4,'d','2024-06-01T10:00:00+02:00');
         UPDATE tasks SET title='cc' WHERE id=3; UPDATE tasks SET title='c' WHERE id=3;
         INSERT INTO untracked VALUES('ignored: no primary key');
         SAVEPOINT s; UPDATE tasks SET title='rolled back' WHERE id=1; ROLLBACK TO s; RELEASE s",
    )
    .unwrap();
    let null = || Some(Value::Null);
    assert_eq!(
        recorded(&mut db),
        vec![
            Change {
                table: "tasks".into(),
                primary_key: vec![true, false, false, false, false],
                operation: Operation::Update,
                indirect: false,
                old: Some(vec![
                    Some(Value::Integer(1)),
                    Some(Value::Text("a".into())),
                    // Storage cells hold normalized decimals; column scale is metadata.
                    Some(Value::Decimal("1.5".into())),
                    None,
                    None
                ]),
                new: Some(vec![
                    None,
                    Some(Value::Text("A".into())),
                    Some(Value::Decimal("2.25".into())),
                    None,
                    None
                ]),
            },
            Change {
                table: "tasks".into(),
                primary_key: vec![true, false, false, false, false],
                operation: Operation::Delete,
                indirect: false,
                old: Some(vec![
                    Some(Value::Integer(2)),
                    Some(Value::Text("b".into())),
                    null(),
                    null(),
                    null()
                ]),
                new: None,
            },
            Change {
                table: "tasks".into(),
                primary_key: vec![true, false, false, false, false],
                operation: Operation::Insert,
                indirect: false,
                old: None,
                new: Some(vec![
                    Some(Value::Integer(4)),
                    Some(Value::Text("d".into())),
                    null(),
                    Some(Value::Instant("2024-06-01T08:00:00.000000Z".into())),
                    null()
                ]),
            },
        ]
    );
    // The changeset reflects the transaction so far; later writes extend it.
    db.execute("DELETE FROM tasks WHERE id=4").unwrap();
    assert_eq!(recorded(&mut db).len(), 2);
    db.execute("COMMIT").unwrap();
    assert!(db.execute("SELECT boxdb_changeset()").is_err());
    db.execute("BEGIN; SELECT boxdb_record(1); UPDATE tasks SET title='x'; ROLLBACK")
        .unwrap();
    db.execute("BEGIN; SELECT boxdb_record(1)").unwrap();
    assert_eq!(recorded(&mut db), vec![]);
    db.execute("ROLLBACK").unwrap();
}

fn history(db: &mut boxdb::Database) -> Vec<(Value, Vec<Operation>)> {
    db.execute("SELECT label, changes FROM history ORDER BY id")
        .unwrap()[0]
        .rows
        .iter()
        .map(|row| {
            let Value::Changeset(bytes) = &row[1] else {
                panic!("expected a changeset, got {:?}", row[1]);
            };
            (
                row[0].clone(),
                changeset::decode(bytes)
                    .unwrap()
                    .iter()
                    .map(|change| change.operation)
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn commit_triggers_record_committing_transactions_into_user_tables() {
    let mut db = boxdb::Database::memory();
    db.execute(
        "CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT NOT NULL);
         CREATE TABLE notes(id INT64 PRIMARY KEY, body TEXT);
         CREATE TABLE history(id INT64 PRIMARY KEY, label TEXT, changes CHANGESET NOT NULL);
         CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
           INSERT INTO history(label, changes) VALUES(boxdb_context('label'), boxdb_changeset());
         END",
    )
    .unwrap();
    db.execute("INSERT INTO tasks VALUES(1,'a')").unwrap();
    db.execute("BEGIN; SELECT boxdb_set_context('label','rename'); UPDATE tasks SET title='b' WHERE id=1; COMMIT").unwrap();
    db.execute("BEGIN; SELECT count(*) FROM tasks; COMMIT")
        .unwrap();
    db.execute("INSERT INTO notes VALUES(1,'not recorded')")
        .unwrap();
    db.execute("SAVEPOINT outer; DELETE FROM tasks WHERE id=1; RELEASE outer")
        .unwrap();
    db.execute("BEGIN; INSERT INTO tasks VALUES(9,'z'); ROLLBACK")
        .unwrap();
    db.execute("BEGIN; SELECT boxdb_record(0); INSERT INTO tasks VALUES(8,'skipped'); COMMIT")
        .unwrap();
    assert_eq!(
        history(&mut db),
        vec![
            (Value::Null, vec![Operation::Insert]),
            (Value::Text("rename".into()), vec![Operation::Update]),
            (Value::Null, vec![Operation::Delete]),
        ]
    );
    // Context does not leak into later transactions.
    db.execute("BEGIN; INSERT INTO tasks VALUES(2,'c'); COMMIT")
        .unwrap();
    assert_eq!(history(&mut db).last().unwrap().0, Value::Null);

    db.execute(
        "CREATE TABLE audit(n INT64 NOT NULL CHECK(n>0));
         CREATE TRIGGER failing BEFORE COMMIT ON tasks BEGIN INSERT INTO audit VALUES(0); END",
    )
    .unwrap();
    db.execute("BEGIN; INSERT INTO tasks VALUES(3,'d')")
        .unwrap();
    let error = db.execute("COMMIT").unwrap_err();
    assert!(error.to_string().contains("failing"), "{error}");
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_CHECK");
    assert!(db.in_transaction());
    db.execute("ROLLBACK").unwrap();
    assert!(db.execute("INSERT INTO tasks VALUES(3,'d')").is_err());
    assert!(!db.in_transaction());
    assert_eq!(
        db.execute("SELECT count(*) FROM tasks").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(history(&mut db).len(), 4);
    db.execute("DROP TRIGGER failing").unwrap();
    db.execute("INSERT INTO tasks VALUES(3,'d')").unwrap();
    assert_eq!(history(&mut db).len(), 5);

    assert!(db.execute("DROP TRIGGER missing").is_err());
    db.execute("DROP TRIGGER IF EXISTS missing").unwrap();
    assert!(
        db.execute("CREATE TRIGGER record_history BEFORE COMMIT BEGIN SELECT 1; END")
            .is_err()
    );
    db.execute("CREATE TRIGGER IF NOT EXISTS record_history BEFORE COMMIT BEGIN SELECT 1; END")
        .unwrap();
    for sql in [
        "CREATE TEMP TRIGGER t BEFORE COMMIT BEGIN SELECT 1; END",
        "CREATE TRIGGER t BEFORE COMMIT ON temp.tasks BEGIN SELECT 1; END",
        "CREATE TRIGGER t BEFORE COMMIT ON missing BEGIN SELECT 1; END",
        "CREATE TRIGGER t AFTER COMMIT BEGIN SELECT 1; END",
        "CREATE TRIGGER t BEFORE COMMIT BEGIN SELECT FROM; END",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    // Ordinary row triggers still belong to SQLite.
    db.execute("CREATE TRIGGER row_trigger AFTER INSERT ON notes BEGIN UPDATE notes SET body=body||'!' WHERE id=NEW.id; END; DROP TRIGGER row_trigger").unwrap();
}

#[test]
fn commit_triggers_persist_in_metadata_and_dumps() {
    let temp = tempfile::tempdir().unwrap();
    for format in [boxdb::Format::Binary, boxdb::Format::Readable] {
        let path = temp.path().join(format!("data.{format:?}"));
        let mut db = boxdb::Database::open(&path, format).unwrap();
        db.execute(
            "CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT);
             CREATE TABLE history(id INT64 PRIMARY KEY, changes CHANGESET);
             CREATE TRIGGER record_history BEFORE COMMIT ON main.tasks BEGIN
               INSERT INTO history(changes) VALUES(boxdb_changeset());
             END",
        )
        .unwrap();
        drop(db);
        let mut db = boxdb::Database::open(&path, format).unwrap();
        db.execute("INSERT INTO tasks VALUES(1,'a')").unwrap();
        assert_eq!(
            db.execute("SELECT count(*) FROM history").unwrap()[0].rows,
            vec![vec![Value::Integer(1)]]
        );
        let sql = String::from_utf8(db.dump_schema(boxdb::SchemaFormat::Sql).unwrap()).unwrap();
        assert!(
            sql.contains("CREATE TRIGGER record_history BEFORE COMMIT ON main.tasks"),
            "{sql}"
        );
        let yaml = String::from_utf8(db.dump_schema(boxdb::SchemaFormat::Yaml).unwrap()).unwrap();
        assert!(yaml.contains("commit_triggers:"), "{yaml}");
        assert!(yaml.contains("record_history:"), "{yaml}");
        db.execute("ALTER TABLE tasks RENAME TO todo; INSERT INTO todo VALUES(2,'b')")
            .unwrap();
        assert_eq!(
            db.execute("SELECT count(*) FROM history").unwrap()[0].rows,
            vec![vec![Value::Integer(2)]]
        );
    }
}

fn tasks(db: &mut boxdb::Database) -> Vec<Vec<Value>> {
    db.execute("SELECT id, title, budget FROM tasks ORDER BY id")
        .unwrap()[0]
        .rows
        .clone()
}
fn stored_changeset(db: &mut boxdb::Database, id: i64) -> Vec<u8> {
    match &db
        .execute_params(
            "SELECT changes FROM history WHERE id=?",
            &[Value::Integer(id)],
        )
        .unwrap()[0]
        .rows[0][0]
    {
        Value::Changeset(bytes) => bytes.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn applying_inverted_changesets_undoes_transactions_and_records_the_undo() {
    use boxdb::changeset::{ApplyOptions, ConflictKind, ConflictPolicy};
    let mut db = boxdb::Database::memory();
    db.execute(
        "CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT NOT NULL, budget DECIMAL(12,2));
         CREATE TABLE history(id INT64 PRIMARY KEY, label TEXT, changes CHANGESET NOT NULL);
         CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
           INSERT INTO history(changes) VALUES(boxdb_changeset());
         END;
         INSERT INTO tasks VALUES(1,'a','1.00'),(2,'b',NULL)",
    )
    .unwrap();
    db.execute("BEGIN; UPDATE tasks SET title='A', budget='2.50' WHERE id=1; DELETE FROM tasks WHERE id=2; INSERT INTO tasks VALUES(3,'c',NULL); COMMIT").unwrap();
    let original = vec![
        vec![
            Value::Integer(1),
            Value::Text("a".into()),
            Value::Decimal("1.00".into()),
        ],
        vec![Value::Integer(2), Value::Text("b".into()), Value::Null],
    ];
    assert_ne!(tasks(&mut db), original);
    assert!(
        db.execute(
            "SELECT boxdb_apply((SELECT changes FROM history WHERE id=2), '{\"invert\":true}')"
        )
        .is_err(),
        "apply requires a transaction"
    );
    db.execute("BEGIN").unwrap();
    let report = db
        .execute(
            "SELECT boxdb_apply((SELECT changes FROM history WHERE id=2), '{\"invert\":true}')",
        )
        .unwrap()[0]
        .rows[0][0]
        .clone();
    db.execute("COMMIT").unwrap();
    assert_eq!(
        report,
        Value::Text("{\"applied\":3,\"skipped\":0,\"conflicts\":[]}".into())
    );
    assert_eq!(tasks(&mut db), original);
    assert_eq!(
        history(&mut db).last().unwrap().1,
        vec![Operation::Update, Operation::Insert, Operation::Delete]
    );

    // Redo through the Rust API: the undo's own inversion restores the change.
    let undo = stored_changeset(&mut db, 3);
    let report = db
        .apply_changeset(
            &undo,
            ApplyOptions {
                invert: true,
                on_conflict: ConflictPolicy::Abort,
            },
        )
        .unwrap();
    assert_eq!((report.applied, report.skipped), (3, 0));
    assert_eq!(
        tasks(&mut db)[0],
        vec![
            Value::Integer(1),
            Value::Text("A".into()),
            Value::Decimal("2.50".into())
        ]
    );
    assert_eq!(history(&mut db).len(), 4);

    // A later edit conflicts with undoing the older change.
    db.execute("UPDATE tasks SET title='edited' WHERE id=1")
        .unwrap();
    let second = stored_changeset(&mut db, 2);
    let error = db
        .apply_changeset(
            &second,
            ApplyOptions {
                invert: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("conflict"), "{error}");
    assert_eq!(tasks(&mut db)[0][1], Value::Text("edited".into()));
    let report = db
        .apply_changeset(
            &second,
            ApplyOptions {
                invert: true,
                on_conflict: ConflictPolicy::Skip,
            },
        )
        .unwrap();
    assert_eq!((report.applied, report.skipped), (2, 1));
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, ConflictKind::Data);
    assert_eq!(report.conflicts[0].table, "tasks");
    assert_eq!(report.conflicts[0].key, vec![serde_json::json!(1)]);
    assert_eq!(tasks(&mut db)[0][1], Value::Text("edited".into()));
    let report = db
        .apply_changeset(
            &second,
            ApplyOptions {
                invert: true,
                on_conflict: ConflictPolicy::Replace,
            },
        )
        .unwrap();
    assert_eq!((report.applied, report.skipped), (1, 2));
    assert_eq!(tasks(&mut db)[0][1], Value::Text("a".into()));
    assert!(
        db.apply_changeset(b"garbage", ApplyOptions::default())
            .is_err()
    );
}

#[test]
fn changeset_rows_lists_fields_with_current_column_names() {
    let mut db = boxdb::Database::memory();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY, name TEXT)")
        .unwrap();
    let rows = db
        .execute_params(
            "SELECT seq, \"table\", operation, position, column_name, primary_key, old, new FROM changeset_rows(?) ORDER BY seq, position",
            &[Value::Changeset(update_bytes())],
        )
        .unwrap()[0]
        .rows
        .clone();
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Integer(0),
                Value::Text("t".into()),
                Value::Text("update".into()),
                Value::Integer(0),
                Value::Text("id".into()),
                Value::Integer(1),
                Value::Integer(1),
                Value::Null
            ],
            vec![
                Value::Integer(0),
                Value::Text("t".into()),
                Value::Text("update".into()),
                Value::Integer(1),
                Value::Text("name".into()),
                Value::Integer(0),
                Value::Text("a".into()),
                Value::Text("b".into())
            ],
        ]
    );
    assert_eq!(
        db.execute("SELECT changeset_invert(NULL), changeset_concat(NULL, NULL)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Null, Value::Null]]
    );
    assert_eq!(
        db.execute_params(
            "SELECT count(*) FROM changeset_rows(changeset_invert(?))",
            &[Value::Changeset(update_bytes())],
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn readable_storage_writes_changesets_as_structured_documents() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = boxdb::Database::open(&path, boxdb::Format::Readable).unwrap();
    db.execute(
        "CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT, budget DECIMAL(12,2));
         CREATE TABLE history(id INT64 PRIMARY KEY, changes CHANGESET);
         CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
           INSERT INTO history(changes) VALUES(boxdb_changeset());
         END;
         INSERT INTO tasks VALUES(1,'a','1.50')",
    )
    .unwrap();
    db.execute("UPDATE tasks SET title='b' WHERE id=1").unwrap();
    let expected = db
        .execute("SELECT changes FROM history ORDER BY id")
        .unwrap();
    drop(db);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("changeset:"), "{text}");
    assert!(text.contains("table: tasks"), "{text}");
    assert!(text.contains("decimal: \"1.5\""), "{text}");
    assert!(text.contains("op: update"), "{text}");
    assert!(!text.contains("blob:"), "{text}");
    let mut db = boxdb::Database::open(&path, boxdb::Format::Readable).unwrap();
    assert_eq!(
        db.execute("SELECT changes FROM history ORDER BY id")
            .unwrap(),
        expected
    );
    // Undo still works after the round trip.
    let bytes = stored_changeset(&mut db, 2);
    db.apply_changeset(
        &bytes,
        boxdb::changeset::ApplyOptions {
            invert: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT title FROM tasks").unwrap()[0].rows,
        vec![vec![Value::Text("a".into())]]
    );
}
