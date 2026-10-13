use boxdb::{Database, Format, Value};

#[test]
fn binary_snapshots_use_boxdb_header_and_read_previous_format() {
    // Captured from the pre-release binary format before the product rename.
    let legacy = hex::decode("4c49474854444200017dcdee69816f7e9f6f3e0db7fd6bba1b87eeeaa03b29cd9c7fb7d24538b5249328b52ffd0058950500e28b2723306dab03861109d25c64ad2068448cf29af9c2280df3503a7728af9ad3890a0ac4300c8ba7e1797c1070821dccc80337ba7d1f21adeee893ec9b6e7474cb1d863dec8ac68e6bb25f4cff78cd03d4025219598f4965208364240561490a99500152094b2e0690890e061495482928117cdf28e05560bfd8e3350e11d2ee7ad8f1b9ef5b82ddf5dd188e8b27d9bbc760d38eeb1b32c391d7f7cbb1883b010500706c7047d68d83bd6cea428c423d01").unwrap();
    let mut db = Database::from_snapshot(&legacy).unwrap();
    assert_eq!(
        db.execute("SELECT id,body FROM notes").unwrap()[0].rows,
        vec![vec![
            Value::Integer(1),
            Value::Text("legacy snapshot".into())
        ]]
    );
    assert!(
        db.export(Format::Binary)
            .unwrap()
            .starts_with(b"BOXDB\0\x01")
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("restored.boxdb");
    drop(Database::restore(&path, &legacy).unwrap());
    assert!(std::fs::read(&path).unwrap().starts_with(b"BOXDB\0\x01"));
    let mut reopened = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        reopened.execute("SELECT body FROM notes").unwrap()[0].rows,
        vec![vec![Value::Text("legacy snapshot".into())]]
    );
}

#[test]
fn both_formats_reopen_committed_typed_rows_and_discard_uncommitted_changes() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.boxdb");
        {
            let mut db = Database::open(&path, format).unwrap();
            db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n NUMERIC(20,4),tags TEXT[],created DATE); INSERT INTO t VALUES(1,1234567890123456.1234,ARRAY['a'],DATE '2024-02-29'); BEGIN; UPDATE t SET n=2; COMMIT; BEGIN; UPDATE t SET n=3").unwrap();
            assert!(Database::open(&path, format).is_err());
        }
        let mut db = Database::open(&path, Format::Binary).unwrap();
        assert_eq!(
            db.execute("SELECT n,tags,created FROM t").unwrap()[0].rows,
            vec![vec![
                Value::Numeric("2.0000".into()),
                Value::Array(vec![Value::Text("a".into())]),
                Value::Date("2024-02-29".into())
            ]]
        );
    }
}

#[test]
fn releasing_outer_implicit_savepoint_commits_to_disk() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("savepoints.boxdb");
    {
        let mut db = Database::open(&path, Format::Binary).unwrap();
        db.execute("CREATE TABLE t(n INTEGER); SAVEPOINT outermost; INSERT INTO t VALUES(1); SAVEPOINT inner; INSERT INTO t VALUES(2); RELEASE inner; RELEASE outermost").unwrap();
        assert!(!db.in_transaction());
    }
    let mut db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn readable_sql_schema_is_authoritative_for_typed_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("schema.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
            .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("CREATE TABLE t (n INTEGER)", "CREATE TABLE t (n TEXT)");
    std::fs::write(&path, text).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
}

#[test]
fn foreign_keys_reopen_independently_of_table_name_order() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relations.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE zparent(n INTEGER PRIMARY KEY); CREATE TABLE achild(n INTEGER REFERENCES zparent(n) ON DELETE CASCADE); INSERT INTO zparent VALUES(1); INSERT INTO achild VALUES(1)").unwrap();
    }
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("DELETE FROM zparent").unwrap();
    assert!(
        db.execute("SELECT n FROM achild").unwrap()[0]
            .rows
            .is_empty()
    );
}

#[test]
fn foreign_keys_can_reopen_when_the_parent_key_is_a_separate_unique_index() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("indexes.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE parent(n INTEGER); CREATE UNIQUE INDEX parent_n ON parent(n); CREATE TABLE child(n INTEGER REFERENCES parent(n)); INSERT INTO parent VALUES(1); INSERT INTO child VALUES(1)").unwrap();
    }
    let mut db = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        db.execute("SELECT n FROM child").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn readable_value_normalization_cannot_bypass_unique_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("uuids.jsonl");
    let first = "550e8400-e29b-41d4-a716-446655440000";
    let second = "550e8400-e29b-41d4-a716-446655440001";
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(u UUID UNIQUE)").unwrap();
        db.execute_params(
            "INSERT INTO t VALUES(?),(?)",
            &[Value::Uuid(first.into()), Value::Uuid(second.into())],
        )
        .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace(second, &first.to_uppercase());
    std::fs::write(&path, text).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
}

#[test]
fn malformed_readable_constraints_return_errors_without_panicking() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.jsonl");
    {
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute("CREATE TABLE t(n INTEGER UNIQUE); INSERT INTO t VALUES(1)")
            .unwrap();
    }
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("n INTEGER UNIQUE", "n INTEGER CHECK(missing>0)");
    std::fs::write(&path, text).unwrap();
    assert!(Database::open(&path, Format::Readable).is_err());
}

#[test]
fn format_conversion_and_snapshot_restore_preserve_only_committed_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("restored.boxdb");
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER UNIQUE); INSERT INTO t VALUES(1); CREATE INDEX t_n ON t(n); BEGIN; INSERT INTO t VALUES(2)").unwrap();
    let bytes = db.export(Format::Readable).unwrap();
    let mut restored = Database::restore(&path, &bytes).unwrap();
    assert_eq!(restored.format(), Some(Format::Readable));
    assert_eq!(
        restored.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
    assert!(Database::restore(&path, &bytes).is_err());
    let binary = restored.export(Format::Binary).unwrap();
    assert_ne!(binary, bytes);
    let second = temp.path().join("second.boxdb");
    let mut reopened = Database::restore(&second, &binary).unwrap();
    assert!(reopened.execute("INSERT INTO t VALUES(1)").is_err());
    assert_eq!(
        reopened
            .execute("SELECT name FROM sqlite_schema WHERE type='index'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("t_n".into())]]
    );
    let invalid = temp.path().join("invalid.boxdb");
    assert!(Database::restore(&invalid, b"invalid").is_err());
    assert!(!invalid.exists());
}

#[test]
fn readable_updates_change_one_row_and_deletes_keep_other_row_ids_stable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.jsonl");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1),(2),(3)")
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    db.execute("UPDATE t SET n=20 WHERE n=2").unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count(),
        1
    );
    let last = after.lines().last().unwrap().to_string();
    db.execute("DELETE FROM t WHERE n=1").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
        last
    );
}

#[test]
fn corrupted_files_are_rejected_without_overwriting_them() {
    for format in [Format::Binary, Format::Readable] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.boxdb");
        {
            let mut db = Database::open(&path, format).unwrap();
            db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
                .unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        if format == Format::Binary {
            bytes[10] ^= 1;
        } else {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace("{\"Integer\":1}", "{\"Text\":\"bad\"}")
                .into_bytes();
        }
        std::fs::write(&path, &bytes).unwrap();
        assert!(Database::open(&path, format).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn failed_file_publication_does_not_change_live_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    std::fs::rename(&path, temp.path().join("saved.boxdb")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(db.execute("UPDATE t SET n=2").is_err());
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}
