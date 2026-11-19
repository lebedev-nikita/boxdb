//! Readable files append one commit document per transaction and compact
//! into a canonical snapshot when the journal outgrows it.
#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{CheckMode, Database, Format, OpenOptions, Value};
use std::io::Write;

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn append(path: &std::path::Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// Rows that make the snapshot large enough to keep a journal.
const FILLER: &str = "CREATE TABLE filler(id INTEGER PRIMARY KEY,body TEXT); WITH RECURSIVE r(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM r WHERE n<100) INSERT INTO filler SELECT n,'filler row '||n FROM r";

/// The logical content of a readable file as a canonical snapshot.
fn canonical(path: &std::path::Path) -> Vec<u8> {
    let mut db = files::open_bytes(&std::fs::read(path).unwrap()).unwrap();
    files::converted_bytes(&mut db, Format::Readable).unwrap()
}

#[test]
fn transactions_append_commit_documents_after_the_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notes.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE notes(id INTEGER PRIMARY KEY,body TEXT); CREATE TABLE seq(id INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT)").unwrap();
    db.execute("INSERT INTO notes VALUES(1,'first'),(2,'second')")
        .unwrap();
    let snapshot = read(&path);
    assert!(snapshot.ends_with("...\n"), "{snapshot}");
    db.execute("INSERT INTO notes VALUES(3,'third')").unwrap();
    let first = read(&path);
    assert_eq!(
        &first[snapshot.len()..],
        "---\nkind: commit\ntables:\n  notes:\n    put:\n    - {rowid: 3, body: third, id: 3}\n...\n"
    );
    db.execute("BEGIN; UPDATE notes SET body='Line one'||char(10)||'line two' WHERE id=1; DELETE FROM notes WHERE id=2; INSERT INTO seq(body) VALUES('x'); COMMIT").unwrap();
    let second = read(&path);
    assert!(second.starts_with(&first));
    assert_eq!(
        &second[first.len()..],
        "---\nkind: commit\ntables:\n  notes:\n    put:\n    - rowid: 1\n      body: |-\n        Line one\n        line two\n      id: 1\n    delete: [2]\n  seq:\n    put:\n    - {rowid: 1, body: x, id: 1}\nsequences: {seq: 1}\n...\n"
    );
    let expected = db.execute("SELECT * FROM notes ORDER BY id").unwrap();
    drop(db);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        reopened.execute("SELECT * FROM notes ORDER BY id").unwrap(),
        expected
    );
    reopened
        .execute("INSERT INTO seq(body) VALUES('y')")
        .unwrap();
    let third = read(&path);
    assert!(third.starts_with(&second), "reopening keeps appending");
    assert!(third.ends_with("sequences: {seq: 2}\n...\n"), "{third}");
    reopened.execute("SELECT * FROM notes").unwrap();
    reopened.execute("BEGIN; COMMIT").unwrap();
    assert_eq!(read(&path), third, "unchanged transactions append nothing");
}

/// Update one row with ~1 KiB commits until the file is rewritten. Returns the
/// snapshot size, the largest file seen and whether the journal exceeded half
/// of the snapshot.
fn compact(rows: usize) -> (usize, usize, bool) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("counter.yaml");
    let setup = format!(
        "CREATE TABLE counter(id INTEGER PRIMARY KEY,body TEXT); WITH RECURSIVE r(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM r WHERE n<{rows}) INSERT INTO counter SELECT n,'row '||n FROM r"
    );
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(&setup).unwrap();
    let snapshot = read(&path).len();
    let mut largest = snapshot;
    for step in 1..1000 {
        let body = Value::Text(format!("{step} {}", "x".repeat(1000)));
        let update = "UPDATE counter SET body=? WHERE id=1";
        db.execute_params(update, std::slice::from_ref(&body))
            .unwrap();
        let text = read(&path);
        if text.contains("kind: commit") {
            largest = largest.max(text.len());
            continue;
        }
        let mut reference = Database::memory();
        reference.execute(&setup).unwrap();
        reference.execute_params(update, &[body]).unwrap();
        assert_eq!(
            text.as_bytes(),
            files::converted_bytes(&mut reference, Format::Readable).unwrap()
        );
        return (snapshot, largest, largest - snapshot > snapshot / 2);
    }
    panic!("the journal was never compacted");
}

#[test]
fn small_files_keep_up_to_64_kib_of_journal() {
    let (snapshot, largest, beyond_half) = compact(20);
    assert!(beyond_half, "{snapshot} {largest}");
    assert!(largest > snapshot + 60 * 1024, "{snapshot} {largest}");
    assert!(
        largest <= snapshot + 64 * 1024 + 2048,
        "{snapshot} {largest}"
    );
}

#[test]
fn large_files_keep_up_to_half_of_the_snapshot_as_journal() {
    let (snapshot, largest, beyond_half) = compact(20_000);
    assert!(snapshot > 256 * 1024, "{snapshot}");
    assert!(!beyond_half, "{snapshot} {largest}");
    assert!(largest > snapshot * 3 / 2 - 4096, "{snapshot} {largest}");
}

#[test]
fn rolled_back_changes_are_not_appended() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); CREATE TRIGGER reject AFTER INSERT ON t WHEN NEW.body='bad' BEGIN SELECT RAISE(ABORT,'bad row'); END").unwrap();
    db.execute("INSERT INTO t VALUES(1,'a')").unwrap();
    let before = read(&path);
    db.execute("BEGIN; UPDATE t SET body='discarded' WHERE id=1; INSERT INTO t VALUES(2,'discarded'); ROLLBACK").unwrap();
    assert!(db.execute("INSERT INTO t VALUES(3,'bad')").is_err());
    db.execute("INSERT INTO t VALUES(4,'d')").unwrap();
    let text = read(&path);
    assert_eq!(
        &text[before.len()..],
        "---\nkind: commit\ntables:\n  t:\n    put:\n    - {rowid: 4, body: d, id: 4}\n...\n"
    );
}

#[test]
fn schema_and_header_changes_rewrite_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    for sql in [
        "PRAGMA user_version=3",
        "ALTER TABLE t ADD COLUMN extra TEXT",
        "VACUUM",
    ] {
        db.execute("INSERT INTO t(body) VALUES('more')").unwrap();
        assert!(read(&path).contains("kind: commit"), "{sql}");
        db.execute(sql).unwrap();
        let text = read(&path);
        assert!(!text.contains("kind: commit"), "{sql}: {text}");
        assert_eq!(text.as_bytes(), canonical(&path), "{sql}");
    }
}

#[test]
fn replay_restores_swaps_cascades_and_rowid_changes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    let mut reference = Database::memory();
    for sql in [
        FILLER,
        "CREATE TABLE parent(id INTEGER PRIMARY KEY,label TEXT UNIQUE); CREATE TABLE child(id INTEGER PRIMARY KEY,p INTEGER REFERENCES parent(id) ON DELETE CASCADE ON UPDATE CASCADE); CREATE TABLE plain(body TEXT UNIQUE)",
        "INSERT INTO parent VALUES(1,'a'),(2,'b'),(3,'c'); INSERT INTO child VALUES(1,1),(2,2),(3,3); INSERT INTO plain VALUES('x'),('y')",
        "BEGIN; UPDATE parent SET label='tmp' WHERE id=1; UPDATE parent SET label='a' WHERE id=2; UPDATE parent SET label='b' WHERE id=1; COMMIT",
        "BEGIN; UPDATE child SET p=3 WHERE id=1; DELETE FROM parent WHERE id=1; COMMIT",
        "DELETE FROM parent WHERE id=2",
        "UPDATE parent SET id=30 WHERE id=3",
        "INSERT OR REPLACE INTO parent VALUES(40,'c')",
        "UPDATE plain SET rowid=rowid+10",
        "BEGIN; INSERT INTO plain VALUES('z'); SAVEPOINT s; DELETE FROM plain; ROLLBACK TO s; RELEASE s; COMMIT",
    ] {
        db.execute(sql).unwrap();
        reference.execute(sql).unwrap();
        assert_eq!(
            canonical(&path),
            files::converted_bytes(&mut reference, Format::Readable).unwrap(),
            "{sql}"
        );
    }
    assert!(read(&path).contains("kind: commit"));
}

const TORN: &str = "---\nkind: commit\ntables:\n  t:\n    put:\n    - {rowid: 9, bo";

#[test]
fn interrupted_appends_are_ignored_and_removed_by_the_next_writer() {
    for tail in [TORN.as_bytes(), b"---\nkind: com", &[0u8; 64]] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.yaml");
        let mut db = Database::open(&path, Format::Readable).unwrap();
        db.execute(FILLER).unwrap();
        db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
            .unwrap();
        db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
        drop(db);
        let committed = read(&path);
        append(&path, tail);
        let report = Database::check_file(&path, CheckMode::Full).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "BOXDB_INCOMPLETE_COMMIT"),
            "{report:?}"
        );
        let mut reader = Database::open_with_options(
            &path,
            Format::Readable,
            OpenOptions {
                read_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            reader.execute("SELECT id FROM t ORDER BY id").unwrap()[0].rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        drop(reader);
        let mut writer = Database::open(&path, Format::Readable).unwrap();
        writer.execute("INSERT INTO t VALUES(3,'c')").unwrap();
        let text = read(&path);
        assert!(text.starts_with(&committed), "{text}");
        assert_eq!(
            &text[committed.len()..],
            "---\nkind: commit\ntables:\n  t:\n    put:\n    - {rowid: 3, body: c, id: 3}\n...\n"
        );
        let report = Database::check_file(&path, CheckMode::Full).unwrap();
        assert!(report.issues.is_empty(), "{report:?}");
    }
}

#[test]
fn damaged_commits_before_the_end_are_errors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    db.execute("INSERT INTO t VALUES(3,'c')").unwrap();
    drop(db);
    let text = read(&path);
    assert_eq!(text.matches("kind: commit\n").count(), 3, "{text}");
    let damaged = text.replacen(
        "{rowid: 2, body: b, id: 2}",
        "{rowid: 2, body: [b, id: 2}",
        1,
    );
    assert_ne!(damaged, text);
    assert!(files::open_bytes(damaged.as_bytes()).is_err());
    let unknown = text.replacen(
        "  t:\n    put:\n    - {rowid: 2",
        "  missing:\n    put:\n    - {rowid: 2",
        1,
    );
    assert!(files::open_bytes(unknown.as_bytes()).is_err());
    let rowless = text.replacen("{rowid: 2, body: b, id: 2}", "{body: b, id: 2}", 1);
    assert!(files::open_bytes(rowless.as_bytes()).is_err());
    let after_commit = format!("{text}---\nkind: sequence\ntable: t\nvalue: 1\n");
    assert!(files::open_bytes(after_commit.as_bytes()).is_err());
}

#[test]
fn recovery_applies_intact_commits_and_rejects_damaged_ones() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let output = temp.path().join("recovered.db");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    db.execute("INSERT INTO t VALUES(3,'c')").unwrap();
    drop(db);
    let text = read(&path);
    assert_eq!(text.matches("kind: commit\n").count(), 3, "{text}");
    let text = text.replacen("{rowid: 2, body: b, id: 2}", "{rowid: 2, body: [b", 1);
    std::fs::write(&path, &text).unwrap();
    let report = Database::recover_file(&path, &output, Format::Binary).unwrap();
    assert_eq!(report.rejected.len(), 1, "{report:?}");
    assert!(
        std::fs::read_to_string(report.rejected_path.unwrap())
            .unwrap()
            .contains("body: [b")
    );
    let mut recovered = Database::open(&output, Format::Binary).unwrap();
    assert_eq!(
        recovered.execute("SELECT id FROM t ORDER BY id").unwrap()[0].rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
}

/// Run the CLI with a file size limit, so the kernel cuts an append short.
/// With SIGXFSZ ignored the next write fails with EFBIG; otherwise the
/// process is killed in the middle of the append.
#[cfg(unix)]
fn run_with_file_limit(
    path: &std::path::Path,
    limit: usize,
    ignore_signal: bool,
    sql: &str,
) -> std::process::Output {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_boxdb"));
    command.arg(path).args(["-e", sql]);
    // SAFETY: the closure runs in the forked child before exec and calls only
    // the async-signal-safe setrlimit and signal functions.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: limit as libc::rlim_t,
                rlim_max: limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if ignore_signal {
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            }
            Ok(())
        });
    }
    command.output().unwrap()
}

#[cfg(unix)]
fn limited_append_setup() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute(FILLER).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    drop(db);
    let before = read(&path);
    assert!(before.contains("kind: commit"), "{before}");
    let sql = format!("INSERT INTO t VALUES(3,'{}')", "x".repeat(8000));
    (temp, path, before, sql)
}

#[cfg(unix)]
fn ids(path: &std::path::Path) -> Vec<Vec<Value>> {
    let mut db = Database::open_with_options(
        path,
        Format::Readable,
        OpenOptions {
            read_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    db.execute("SELECT id FROM t ORDER BY id").unwrap()[0]
        .rows
        .clone()
}

#[cfg(unix)]
#[test]
fn failed_appends_are_truncated_to_the_last_commit() {
    let (_temp, path, before, sql) = limited_append_setup();
    let output = run_with_file_limit(&path, before.len() + 1000, true, &sql);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("append readable commit"), "{stderr}");
    assert_eq!(read(&path), before);
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(report.issues.is_empty(), "{report:?}");
    assert_eq!(
        ids(&path),
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[cfg(unix)]
#[test]
fn appends_cut_short_by_a_killed_process_are_removed_by_the_next_writer() {
    use std::os::unix::process::ExitStatusExt;
    let (_temp, path, before, sql) = limited_append_setup();
    let limit = before.len() + 1000;
    let output = run_with_file_limit(&path, limit, false, &sql);
    assert_eq!(output.status.signal(), Some(libc::SIGXFSZ), "{output:?}");
    // Linux stops at the limit; macOS counts it from the append offset.
    let torn = read(&path);
    assert!(torn.starts_with(&before));
    assert!(torn.len() > before.len() && torn.len() < before.len() + sql.len());
    assert!(!torn.ends_with("...\n"));
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "BOXDB_INCOMPLETE_COMMIT"),
        "{report:?}"
    );
    assert_eq!(
        ids(&path),
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    let mut writer = Database::open(&path, Format::Readable).unwrap();
    writer.execute("INSERT INTO t VALUES(4,'d')").unwrap();
    drop(writer);
    let text = read(&path);
    assert_eq!(
        &text[before.len()..],
        "---\nkind: commit\ntables:\n  t:\n    put:\n    - {rowid: 4, body: d, id: 4}\n...\n"
    );
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(report.issues.is_empty(), "{report:?}");
}
