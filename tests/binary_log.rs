use boxdb::{Database, Format, Value};

#[test]
fn commits_append_only_changed_cells_and_skip_noops() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("log.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER,bytes BYTEA)")
        .unwrap();
    let bytes = vec![42; 600_000];
    db.execute_params(
        "INSERT INTO t VALUES(?,?,?)",
        &[
            Value::Integer(1),
            Value::Integer(1),
            Value::Bytea(bytes.clone()),
        ],
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    db.execute("UPDATE t SET n=2 WHERE id=1").unwrap();
    let updated = std::fs::read(&path).unwrap();
    assert!(updated.starts_with(&before));
    assert!(updated.len() - before.len() < 200);
    db.execute("UPDATE t SET n=2 WHERE id=1").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), updated);
    db.execute("BEGIN; UPDATE t SET n=3 WHERE id=1").unwrap();
    assert_eq!(Database::read_snapshot(&path).unwrap(), updated);
    db.execute("ROLLBACK").unwrap();
    let snapshot = Database::read_snapshot(&path).unwrap();
    let mut replica = Database::from_snapshot(&snapshot).unwrap();
    assert_eq!(
        replica.execute("SELECT n,bytes FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2), Value::Bytea(bytes)]]
    );
    let mut reopened = {
        drop(db);
        Database::open(&path, Format::Binary).unwrap()
    };
    assert_eq!(
        reopened.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn inserting_then_deleting_an_uncommitted_row_preserves_the_id_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sequence.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); BEGIN; INSERT INTO t VALUES(1); DELETE FROM t; COMMIT")
        .unwrap();
    let committed = db.export(Format::Binary).unwrap();
    drop(db);
    let mut db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(db.export(Format::Binary).unwrap(), committed);
    db.execute("INSERT INTO t VALUES(2)").unwrap();
    let readable = String::from_utf8(db.export(Format::Readable).unwrap()).unwrap();
    assert!(readable.contains("\"id\":2"));
}

#[test]
fn signed_zero_updates_are_persisted_in_scalars_arrays_and_json() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("zero.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n DOUBLE PRECISION,a DOUBLE PRECISION[],j JSON)")
        .unwrap();
    for n in [0.0, -0.0] {
        let values = [
            Value::Real(n),
            Value::Array(vec![Value::Real(n)]),
            Value::Json(serde_json::json!({"n":[n]})),
        ];
        if n.to_bits() == 0 {
            db.execute_params("INSERT INTO t VALUES(?,?,?)", &values)
                .unwrap();
        } else {
            db.execute_params("UPDATE t SET n=?,a=?,j=?", &values)
                .unwrap();
        }
    }
    let expected = db.export(Format::Binary).unwrap();
    drop(db);
    let db = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(db.export(Format::Binary).unwrap(), expected);
}

#[test]
fn incomplete_final_commits_recover_to_the_previous_committed_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("recovery.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let previous = std::fs::read(&path).unwrap();
    db.execute("UPDATE t SET n=2").unwrap();
    let latest = std::fs::read(&path).unwrap();
    drop(db);
    let frame = &latest[previous.len()..];
    for cut in [
        1,
        8,
        40,
        41,
        frame.len() / 2,
        frame.len() - 8,
        frame.len() - 1,
    ] {
        let mut torn = previous.clone();
        torn.extend_from_slice(&frame[..cut]);
        std::fs::write(&path, &torn).unwrap();
        assert!(
            Database::from_snapshot(&torn).is_err(),
            "strict import accepted cut {cut}"
        );
        assert_eq!(Database::read_snapshot(&path).unwrap(), previous);
        assert_eq!(std::fs::read(&path).unwrap(), torn);
        let mut db = Database::open(&path, Format::Binary).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        assert_eq!(
            db.execute("SELECT n FROM t").unwrap()[0].rows,
            vec![vec![Value::Integer(1)]]
        );
        db.execute("UPDATE t SET n=3").unwrap();
        let mut replica =
            Database::from_snapshot(&Database::read_snapshot(&path).unwrap()).unwrap();
        assert_eq!(
            replica.execute("SELECT n FROM t").unwrap()[0].rows,
            vec![vec![Value::Integer(3)]]
        );
    }
}

#[test]
fn complete_corrupt_commits_fail_without_truncating_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("corrupt.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let start = std::fs::metadata(&path).unwrap().len() as usize;
    db.execute("UPDATE t SET n=2").unwrap();
    let latest = std::fs::read(&path).unwrap();
    drop(db);
    for position in [
        start,
        start + 1,
        start + 5,
        start + 9,
        start + 41,
        latest.len() - 1,
    ] {
        let mut corrupt = latest.clone();
        corrupt[position] ^= 128;
        std::fs::write(&path, &corrupt).unwrap();
        assert!(Database::read_snapshot(&path).is_err());
        assert!(Database::open(&path, Format::Binary).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }
}

#[test]
fn checkpoint_compacts_the_log_without_losing_the_latest_commit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("checkpoint.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(bytes BYTEA)").unwrap();
    db.execute_params("INSERT INTO t VALUES(?)", &[Value::Bytea(vec![1; 600_000])])
        .unwrap();
    db.execute_params("UPDATE t SET bytes=?", &[Value::Bytea(vec![2; 600_000])])
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        db.export(Format::Binary).unwrap()
    );
    db.execute_params("UPDATE t SET bytes=?", &[Value::Bytea(vec![3; 600_000])])
        .unwrap();
    let expected = db.export(Format::Binary).unwrap();
    drop(db);
    assert_eq!(
        Database::open(&path, Format::Binary)
            .unwrap()
            .export(Format::Binary)
            .unwrap(),
        expected
    );
    // Reopening must retain the checkpoint's original size, rather than treating
    // the entire accumulated log as a new baseline for the growth limit.
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute_params("UPDATE t SET bytes=?", &[Value::Bytea(vec![4; 600_000])])
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        db.export(Format::Binary).unwrap()
    );
}

#[test]
fn snapshot_readers_wait_for_the_publication_lock() {
    use std::{fs::OpenOptions, sync::mpsc, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("locked.boxdb");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let expected = Database::read_snapshot(&path).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(temp.path().join("locked.boxdb.snapshot.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        result_tx.send(Database::read_snapshot(path)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fs2::FileExt::unlock(&lock).unwrap();
    assert_eq!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        expected
    );
    reader.join().unwrap();
}
