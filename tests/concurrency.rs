use boxdb::{Database, Format, Value};
use std::{
    sync::{
        Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::Duration,
};

static COMMIT: Mutex<Option<(SyncSender<()>, Receiver<()>)>> = Mutex::new(None);

fn commit_other_client(attempt: i32) -> bool {
    if attempt == 0 {
        let channels = COMMIT.lock().unwrap();
        let (commit, committed) = channels.as_ref().unwrap();
        commit.send(()).unwrap();
        committed.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    attempt < 5
}

#[test]
fn a_waiting_writer_uses_the_schema_committed_by_another_client() {
    for (sql, valid) in [
        ("INSERT INTO t VALUES('invalid-json')", false),
        (
            "WITH value(j) AS(VALUES('invalid-json')) INSERT INTO t SELECT j FROM value RETURNING j",
            false,
        ),
        ("INSERT INTO t VALUES('{}')", true),
    ] {
        waiting_writer_reloads_schema(sql, valid);
    }
}

fn waiting_writer_reloads_schema(sql: &str, valid: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut waiting = Database::open(&path, Format::Binary).unwrap();
    waiting.execute("CREATE TABLE t(j TEXT)").unwrap();
    let (commit, receive_commit) = sync_channel(1);
    let (committed, receive_committed) = sync_channel(1);
    let (ready, receive_ready) = sync_channel(1);
    *COMMIT.lock().unwrap() = Some((commit, receive_committed));
    waiting.busy_handler(Some(commit_other_client)).unwrap();
    let other = std::thread::spawn(move || {
        let mut db = Database::open(path, Format::Binary).unwrap();
        db.execute("BEGIN IMMEDIATE; DROP TABLE t; CREATE TABLE t(j JSON)")
            .unwrap();
        ready.send(()).unwrap();
        receive_commit
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        db.execute("COMMIT").unwrap();
        committed.send(()).unwrap();
    });
    receive_ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let insert = waiting.execute(sql);
    other.join().unwrap();
    COMMIT.lock().unwrap().take();

    assert_eq!(
        insert.is_ok(),
        valid,
        "a JSON write must use the current validator: {insert:?}"
    );
    assert!(!waiting.in_transaction());
    if !valid {
        assert!(
            waiting.execute("SELECT j FROM t").unwrap()[0]
                .rows
                .is_empty()
        );
        waiting.execute("INSERT INTO t VALUES('{}')").unwrap();
    }
    assert_eq!(
        waiting.execute("SELECT j FROM t").unwrap()[0].rows,
        vec![vec![Value::Json(serde_json::json!({}))]]
    );
}

#[test]
fn standalone_write_errors_preserve_sqlite_conflict_and_foreign_key_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INTEGER PRIMARY KEY)").unwrap();
    let error = db
        .execute("INSERT OR FAIL INTO t VALUES(1),(2),(1),(3)")
        .unwrap_err();
    assert_eq!(error.code(), "BOXDB_CONSTRAINT_PRIMARYKEY");
    assert!(!db.in_transaction());
    let mut other = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(
        other.execute("SELECT n FROM t ORDER BY n").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    db.execute("CREATE TABLE child(n INTEGER REFERENCES t(n) DEFERRABLE INITIALLY DEFERRED)")
        .unwrap();
    assert_eq!(
        db.execute("INSERT INTO child VALUES(99)")
            .unwrap_err()
            .code(),
        "BOXDB_CONSTRAINT_FOREIGNKEY"
    );
    assert!(!db.in_transaction());
    assert!(
        other.execute("SELECT n FROM child").unwrap()[0]
            .rows
            .is_empty()
    );
    assert!(
        db.execute("INSERT OR ROLLBACK INTO t VALUES(4),(1)")
            .is_err()
    );
    assert!(!db.in_transaction());
    assert_eq!(
        other.execute("SELECT n FROM t ORDER BY n").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn deferred_transactions_keep_their_schema_snapshot_and_reject_stale_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut reader = Database::open(&path, Format::Binary).unwrap();
    reader
        .execute("CREATE TABLE t(j JSON); INSERT INTO t VALUES('{}')")
        .unwrap();
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    reader.execute("BEGIN DEFERRED").unwrap();
    assert_eq!(
        reader.execute("SELECT j FROM t").unwrap()[0].rows,
        vec![vec![Value::Json(serde_json::json!({}))]]
    );
    writer
        .execute("DROP TABLE t; CREATE TABLE t(j TEXT); INSERT INTO t VALUES('text')")
        .unwrap();
    assert_eq!(
        reader.execute("SELECT j FROM t").unwrap()[0].rows,
        vec![vec![Value::Json(serde_json::json!({}))]]
    );
    let error = reader.execute("INSERT INTO t VALUES('{}')").unwrap_err();
    assert_eq!(
        error.extended_code(),
        Some(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT)
    );
    assert!(reader.in_transaction());
    reader.execute("ROLLBACK").unwrap();
    assert_eq!(
        reader.execute("SELECT j FROM t").unwrap()[0].rows,
        vec![vec![Value::Text("text".into())]]
    );
    reader.execute("INSERT INTO t VALUES('new text')").unwrap();
}

#[test]
fn read_only_schema_refresh_restores_validators_before_a_later_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut reader = Database::open(&path, Format::Binary).unwrap();
    reader.execute("CREATE TABLE t(j TEXT)").unwrap();
    reader.query_only(true).unwrap();
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    writer
        .execute("DROP TABLE t; CREATE TABLE t(j JSON); INSERT INTO t VALUES('{}')")
        .unwrap();
    assert_eq!(
        reader.execute("SELECT j FROM t").unwrap()[0].rows,
        vec![vec![Value::Json(serde_json::json!({}))]]
    );
    reader.query_only(false).unwrap();
    assert!(
        reader
            .execute("INSERT INTO t VALUES('invalid-json')")
            .is_err()
    );
    writer
        .execute("DROP TABLE t; CREATE TABLE t(j JSON)")
        .unwrap();
    assert!(
        reader.execute("SELECT j FROM t").unwrap()[0]
            .rows
            .is_empty()
    );
    // Identical logical types after recreation still require fresh triggers.
    assert!(
        reader
            .execute("INSERT INTO t VALUES('invalid-json')")
            .is_err()
    );
}

#[test]
fn a_readonly_cte_remains_readable_while_another_client_owns_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.db");
    let mut reader = Database::open(&path, Format::Binary).unwrap();
    reader
        .execute("CREATE TABLE t(n INT64); INSERT INTO t VALUES(1)")
        .unwrap();
    let mut writer = Database::open(&path, Format::Binary).unwrap();
    writer
        .execute("BEGIN IMMEDIATE; INSERT INTO t VALUES(2)")
        .unwrap();
    reader.execute("PRAGMA busy_timeout=0").unwrap();
    assert_eq!(
        reader
            .execute("WITH rows AS(SELECT n FROM t) SELECT n FROM rows")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    writer.execute("ROLLBACK").unwrap();
}

#[test]
fn cancelling_a_standalone_write_never_leaks_its_internal_transaction() {
    use std::cell::Cell;
    thread_local! { static REMAINING: Cell<usize> = const { Cell::new(0) }; }
    fn cancelled() -> bool {
        REMAINING.with(|remaining| {
            remaining.set(remaining.get().saturating_sub(1));
            remaining.get() == 0
        })
    }
    let directory = tempfile::tempdir().unwrap();
    let mut db = Database::open(directory.path().join("data.db"), Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INT64)").unwrap();
    for steps in 1..=150 {
        REMAINING.with(|remaining| remaining.set(steps));
        db.progress_handler(1, Some(cancelled)).unwrap();
        let _ = db.execute("INSERT INTO t VALUES(1)");
        db.progress_handler(0, None).unwrap();
        assert!(
            !db.in_transaction(),
            "transaction leaked after {steps} steps"
        );
        db.execute("DELETE FROM t").unwrap();
    }
}

#[test]
fn cancelling_metadata_preparation_keeps_the_connection_idle_and_reusable() {
    use std::cell::Cell;
    thread_local! { static REMAINING: Cell<usize> = const { Cell::new(0) }; }
    fn cancelled() -> bool {
        REMAINING.with(|remaining| {
            remaining.set(remaining.get().saturating_sub(1));
            remaining.get() == 0
        })
    }
    let directory = tempfile::tempdir().unwrap();
    let mut db = Database::open(directory.path().join("data.db"), Format::Binary).unwrap();
    db.execute("CREATE TABLE t(j JSON)").unwrap();
    for steps in 1..=100 {
        REMAINING.with(|remaining| remaining.set(steps));
        db.progress_handler(1, Some(cancelled)).unwrap();
        let _ = db.statement_info("SELECT j FROM t");
        db.progress_handler(0, None).unwrap();
        assert!(
            !db.in_transaction(),
            "transaction leaked after {steps} steps"
        );
        db.execute("SELECT j FROM t").unwrap();
    }
}

#[test]
fn cancelling_metadata_refresh_during_a_select_preserves_the_database() {
    use std::cell::{Cell, RefCell};
    thread_local! { static REMAINING: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static INTERRUPT: RefCell<Option<boxdb::InterruptHandle>> = const { RefCell::new(None) }; }
    fn cancelled() -> bool {
        REMAINING.with(|remaining| {
            remaining.set(remaining.get().saturating_sub(1));
            if remaining.get() == 0 {
                INTERRUPT.with(|interrupt| interrupt.borrow().as_ref().unwrap().interrupt());
            }
        });
        false
    }
    for steps in 1..=150 {
        let mut db = Database::memory();
        db.execute("CREATE TABLE t(j JSON); INSERT INTO t VALUES('{}')")
            .unwrap();
        INTERRUPT.with(|interrupt| *interrupt.borrow_mut() = Some(db.interrupt_handle()));
        REMAINING.with(|remaining| remaining.set(steps));
        db.progress_handler(1, Some(cancelled)).unwrap();
        let _ = db.execute("SELECT j FROM t");
        db.progress_handler(0, None).unwrap();
        assert!(
            !db.in_transaction(),
            "transaction leaked after {steps} steps"
        );
        assert_eq!(
            db.execute("SELECT j FROM t").unwrap()[0].rows,
            vec![vec![Value::Json(serde_json::json!({}))]],
            "database must remain usable after {steps} steps"
        );
    }
}
