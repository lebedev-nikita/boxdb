use std::{
    path::Path,
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_boxdb");

fn cli(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn migration(dir: &Path, version: &str, up: &str, down: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{version}_test.sql")),
        format!("-- migrate:up\n{up}\n-- migrate:down\n{down}\n"),
    )
    .unwrap();
}

#[test]
fn embedded_migrations_share_file_history_and_can_be_rolled_back() {
    use boxdb::{
        Database, Format,
        migrations::{MigrationFile, MigrationSet, MigrationState},
    };
    let temp = tempfile::tempdir().unwrap();
    let filename = "20261015000000_create_events.sql";
    let sql =
        "-- migrate:up\nCREATE TABLE events(at INSTANT);\n-- migrate:down\nDROP TABLE events;\n";
    std::fs::write(temp.path().join(filename), sql).unwrap();
    let embedded = MigrationSet::from_files([MigrationFile {
        filename: filename.into(),
        sql: sql.replace('\n', "\r\n"),
    }])
    .unwrap();
    let files = MigrationSet::read(temp.path()).unwrap();
    for format in [Format::Binary, Format::Readable] {
        let mut db = Database::open(temp.path().join(format!("{format:?}.db")), format).unwrap();
        assert_eq!(embedded.up(&mut db).unwrap()[0].name, "create_events");
        assert!(files.up(&mut db).unwrap().is_empty());
        assert_eq!(files.status(&db).unwrap()[0].state, MigrationState::Applied);
        assert_eq!(
            files.down(&mut db).unwrap().unwrap().version,
            "20261015000000"
        );
        assert_eq!(
            embedded.status(&db).unwrap()[0].state,
            MigrationState::Pending
        );
        assert!(embedded.down(&mut db).unwrap().is_none());
    }
}

#[test]
fn embedded_migration_filenames_must_be_bare_sql_filenames() {
    use boxdb::migrations::{MigrationFile, MigrationSet};
    for filename in [
        "dir/20261015000000_test.sql",
        "../20261015000000_test.sql",
        "20261015000000_test.txt",
        "20261015000000_test",
        "dir\\20261015000000_test.sql",
    ] {
        assert!(
            MigrationSet::from_files([MigrationFile {
                filename: filename.into(),
                sql: "-- migrate:up\nSELECT 1;\n-- migrate:down\n".into(),
            }])
            .is_err(),
            "accepted {filename}"
        );
    }
}

#[test]
fn migrations_require_an_idle_connection_even_when_nothing_is_pending() {
    use boxdb::{Database, migrations::MigrationSet};
    let migrations = MigrationSet::from_files([]).unwrap();
    let mut db = Database::memory();
    db.execute("BEGIN").unwrap();
    for error in [
        migrations.up(&mut db).err(),
        migrations.down(&mut db).err(),
        migrations.status(&db).err(),
    ] {
        assert_eq!(error.unwrap().code(), "BOXDB_VALIDATION");
        assert!(db.in_transaction());
    }
    db.execute("ROLLBACK").unwrap();
    assert!(migrations.up(&mut db).unwrap().is_empty());
}

#[test]
fn interrupted_migration_rolls_back_even_while_the_progress_handler_keeps_cancelling() {
    use boxdb::{
        Database,
        migrations::{MigrationFile, MigrationSet},
    };
    use std::cell::Cell;
    thread_local! { static STEPS: Cell<usize> = const { Cell::new(0) }; }
    fn cancelled() -> bool {
        STEPS.with(|steps| {
            steps.set(steps.get() + 1);
            steps.get() > 100_000
        })
    }
    let migrations = MigrationSet::from_files([MigrationFile {
        filename: "20261015000000_interrupted.sql".into(),
        sql: "-- migrate:up\nCREATE TABLE events(at INSTANT); WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT SUM(x) FROM n;\n-- migrate:down\nDROP TABLE events;".into(),
    }]).unwrap();
    let mut db = Database::memory();
    db.progress_handler(1, Some(cancelled)).unwrap();
    assert_eq!(
        migrations.up(&mut db).unwrap_err().code(),
        "BOXDB_INTERRUPT"
    );
    db.progress_handler(0, None).unwrap();
    assert!(!db.in_transaction());
    assert!(db.execute("SELECT * FROM events").is_err());
}

#[test]
fn migrations_check_cancellation_without_waiting_for_the_sqlite_progress_interval() {
    use boxdb::{
        Database,
        migrations::{MigrationFile, MigrationSet},
    };
    let migrations = MigrationSet::from_files([MigrationFile {
        filename: "20261015000000_cancelled.sql".into(),
        sql: "-- migrate:up\nCREATE TABLE cancelled(body TEXT);\n-- migrate:down\nDROP TABLE cancelled;".into(),
    }]).unwrap();
    let mut db = Database::memory();
    db.progress_handler(i32::MAX, Some(|| true)).unwrap();
    assert_eq!(
        migrations.up(&mut db).unwrap_err().code(),
        "BOXDB_INTERRUPT"
    );
    db.progress_handler(0, None).unwrap();
    assert!(!db.in_transaction());
    assert!(db.execute("SELECT * FROM cancelled").is_err());
}

#[test]
fn cancellation_while_starting_migrations_does_not_leak_a_transaction() {
    use boxdb::{
        Database,
        migrations::{MigrationFile, MigrationSet},
    };
    use std::cell::Cell;
    thread_local! { static REMAINING: Cell<usize> = const { Cell::new(0) }; }
    fn cancelled() -> bool {
        REMAINING.with(|remaining| {
            remaining.set(remaining.get().saturating_sub(1));
            remaining.get() == 0
        })
    }
    let migrations = MigrationSet::from_files([MigrationFile {
        filename: "20261015000000_cancelled.sql".into(),
        sql: "-- migrate:up\nCREATE TABLE cancelled(body TEXT);\n-- migrate:down\nDROP TABLE cancelled;".into(),
    }]).unwrap();
    for steps in 1..=150 {
        let mut db = Database::memory();
        REMAINING.with(|remaining| remaining.set(steps));
        db.progress_handler(1, Some(cancelled)).unwrap();
        let _ = migrations.up(&mut db);
        db.progress_handler(0, None).unwrap();
        assert!(
            !db.in_transaction(),
            "leaked transaction after {steps} steps"
        );
    }
}

#[test]
fn disabled_progress_handlers_do_not_cancel_migrations() {
    use boxdb::{Database, migrations::MigrationSet};
    for operations in [0, -1] {
        let migrations = MigrationSet::from_files([]).unwrap();
        let mut db = Database::memory();
        db.progress_handler(operations, Some(|| true)).unwrap();
        assert!(migrations.up(&mut db).unwrap().is_empty());
    }
}

#[test]
fn up_preserves_extended_types_and_does_not_apply_a_version_twice() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE events(id INT64 PRIMARY KEY,at INSTANT,n DECIMAL(12,2)); INSERT INTO events VALUES(1,'2024-01-01T00:00:00Z','12.50');",
        "DROP TABLE events;",
    );
    for _ in 0..2 {
        success(cli(&[
            "migrate",
            "up",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
    }
    let output = success(cli(&[
        db.to_str().unwrap(),
        "--json",
        "-e",
        "SELECT at,n FROM events;",
    ]));
    let result: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        result["rows"],
        serde_json::json!([[{"Instant":"2024-01-01T00:00:00.000000Z"},{"Decimal":"12.50"}]])
    );
}

#[test]
fn down_rolls_back_only_the_latest_version_in_both_storage_formats() {
    for format in ["binary", "readable"] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("migrations");
        let db = temp.path().join("data");
        migration(
            &dir,
            "20261014000000",
            "CREATE TABLE events(at INSTANT);",
            "DROP TABLE events;",
        );
        migration(
            &dir,
            "20261014000001",
            "ALTER TABLE events ADD COLUMN tags TEXT[];",
            "ALTER TABLE events DROP COLUMN tags;",
        );
        success(cli(&[
            "migrate",
            "up",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
            "--format",
            format,
        ]));
        success(cli(&[
            "migrate",
            "down",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
        success(cli(&[
            db.to_str().unwrap(),
            "-e",
            "INSERT INTO events VALUES('2024-01-01T00:00:00Z');",
        ]));
        let output = success(cli(&[
            db.to_str().unwrap(),
            "--json",
            "-e",
            "SELECT at FROM events;",
        ]));
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            result["rows"][0][0]["Instant"],
            "2024-01-01T00:00:00.000000Z"
        );
        success(cli(&[
            "migrate",
            "down",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
        success(cli(&[
            "migrate",
            "down",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
    }
}

#[test]
fn status_is_read_only_and_check_reports_pending_work() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.yaml");
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE events(at INSTANT);",
        "DROP TABLE events;",
    );
    success(cli(&[
        db.to_str().unwrap(),
        "--format",
        "readable",
        "-e",
        "SELECT 1;",
    ]));
    let before = std::fs::read(&db).unwrap();
    let pending = cli(&[
        "migrate",
        "status",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
        "--json",
        "--check",
    ]);
    assert_eq!(pending.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(result[0]["state"], "pending");
    assert_eq!(std::fs::read(&db).unwrap(), before);
    success(cli(&[
        "migrate",
        "up",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
    let applied = success(cli(&[
        "migrate",
        "status",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
        "--json",
        "--check",
    ]));
    let result: serde_json::Value = serde_json::from_str(&applied).unwrap();
    assert_eq!(result[0]["state"], "applied");
    let missing = temp.path().join("missing.db");
    assert!(
        !cli(&[
            "migrate",
            "status",
            missing.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert!(!missing.exists());
}

#[test]
fn invalid_migration_history_is_rejected_before_any_pending_sql_runs() {
    for corruption in ["changed", "missing", "duplicate", "out_of_order"] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("migrations");
        let db = temp.path().join("data.db");
        migration(
            &dir,
            "20261014000001",
            "CREATE TABLE original(body TEXT);",
            "DROP TABLE original;",
        );
        success(cli(&[
            "migrate",
            "up",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
        migration(
            &dir,
            "20261014000002",
            "CREATE TABLE unexpected(body TEXT);",
            "DROP TABLE unexpected;",
        );
        let original = dir.join("20261014000001_test.sql");
        match corruption {
            "changed" => std::fs::write(&original, "-- migrate:up\nCREATE TABLE changed(body TEXT);\n-- migrate:down\nDROP TABLE original;\n").unwrap(),
            "missing" => std::fs::remove_file(&original).unwrap(),
            "duplicate" => std::fs::copy(&original, dir.join("20261014000001_duplicate.sql")).map(|_| ()).unwrap(),
            "out_of_order" => migration(&dir, "20261014000000", "CREATE TABLE late(body TEXT);", "DROP TABLE late;"),
            _ => unreachable!(),
        }
        for action in ["up", "down", "status"] {
            let output = cli(&[
                "migrate",
                action,
                db.to_str().unwrap(),
                "--dir",
                dir.to_str().unwrap(),
            ]);
            assert!(!output.status.success(), "{corruption}: {action} succeeded");
        }
        let output = success(cli(&[
            db.to_str().unwrap(),
            "--json",
            "-e",
            "SELECT name FROM sqlite_schema WHERE name IN('original','unexpected','late') ORDER BY name;",
        ]));
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["rows"], serde_json::json!([[{"Text":"original"}]]));
    }
}

#[test]
fn migration_files_cannot_override_transaction_management() {
    for forbidden in [
        "BEGIN;",
        "COMMIT;",
        "ROLLBACK;",
        "SAVEPOINT x;",
        "RELEASE x;",
        "END;",
        "VACUUM;",
        "ATTACH ':memory:' AS other;",
        "PRAGMA foreign_keys=OFF;",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("migrations");
        let db = temp.path().join("data.db");
        migration(
            &dir,
            "20261014000000",
            "CREATE TABLE original(body TEXT);",
            "DROP TABLE original;",
        );
        migration(
            &dir,
            "20261014000001",
            &format!("/* custom SQL */ {forbidden}"),
            "SELECT 1;",
        );
        let output = cli(&[
            "migrate",
            "up",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]);
        assert!(!output.status.success(), "accepted {forbidden}");
        assert!(
            !db.exists(),
            "validation of {forbidden} happened after opening the database"
        );
    }
}

#[test]
fn new_creates_distinct_templates_in_the_default_directory_without_a_database() {
    let temp = tempfile::tempdir().unwrap();
    let mut paths = vec![];
    for _ in 0..2 {
        let output = success(
            Command::new(BIN)
                .current_dir(temp.path())
                .args(["migrate", "new", "create_accounts"])
                .output()
                .unwrap(),
        );
        let path = temp.path().join(output.trim());
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.ends_with("_create_accounts.sql"));
        assert_eq!(filename.split('_').next().unwrap().len(), 14);
        let template = std::fs::read_to_string(&path).unwrap();
        assert!(template.contains("-- migrate:up\n"));
        assert!(template.contains("-- migrate:down\n"));
        paths.push(path);
    }
    assert_ne!(paths[0], paths[1]);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn markers_in_strings_and_trigger_bodies_are_not_section_delimiters() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("20261014000000_notes.sql");
    let sql = "-- Heading\n\n-- migrate:up\nCREATE TABLE notes(body TEXT);\nINSERT INTO notes VALUES('first;\n-- migrate:down\nlast');\nCREATE TRIGGER echo AFTER INSERT ON notes WHEN NEW.body='echo' BEGIN\n-- migrate:down\nINSERT INTO notes VALUES('copy;'); END;\n-- migrate:down\nDROP TABLE notes;\n";
    std::fs::write(&file, sql.replace('\n', "\r\n")).unwrap();
    success(cli(&[
        "migrate",
        "up",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
    let output = success(cli(&[
        db.to_str().unwrap(),
        "--json",
        "-e",
        "INSERT INTO notes VALUES('echo'); SELECT body FROM notes ORDER BY rowid;",
    ]));
    let result: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        result["rows"],
        serde_json::json!([[{"Text":"first;\n-- migrate:down\nlast"}],[{"Text":"echo"}],[{"Text":"copy;"}]])
    );
    std::fs::write(&file, sql).unwrap();
    success(cli(&[
        "migrate",
        "status",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
        "--check",
    ]));
    success(cli(&[
        "migrate",
        "down",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
}

#[test]
fn migration_changes_have_machine_readable_results() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE t(body TEXT);",
        "DROP TABLE t;",
    );
    for (action, count) in [("up", 1), ("up", 0), ("down", 1), ("down", 0)] {
        let output = success(cli(&[
            "migrate",
            action,
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
            "--json",
        ]));
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result.as_array().unwrap().len(), count);
        if !result.as_array().unwrap().is_empty() {
            assert_eq!(result[0]["version"], "20261014000000");
        }
    }
}

#[test]
fn empty_sections_cannot_be_mistaken_for_applied_or_rolled_back_migrations() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    migration(&dir, "20261014000000", "-- TODO", "-- TODO");
    assert!(
        !cli(&[
            "migrate",
            "up",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert!(!db.exists());
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE t(body TEXT);",
        "-- irreversible",
    );
    success(cli(&[
        "migrate",
        "up",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
    assert!(
        !cli(&[
            "migrate",
            "down",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap()
        ])
        .status
        .success()
    );
    let output = success(cli(&[
        "migrate",
        "status",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
        "--json",
        "--check",
    ]));
    let statuses: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(statuses[0]["state"], "applied");
}

#[test]
fn trailing_comments_are_allowed_in_migration_sections() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE t(body TEXT);\n-- explanation after SQL",
        "DROP TABLE t;\n/* explanation after rollback */",
    );
    success(cli(&[
        "migrate",
        "up",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
    success(cli(&[
        "migrate",
        "down",
        db.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
}

#[test]
fn failed_up_and_down_leave_the_current_version_and_typed_schema_atomic() {
    for format in ["binary", "readable"] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("migrations");
        let db = temp.path().join("data");
        migration(
            &dir,
            "20261014000000",
            "CREATE TABLE original(at INSTANT);",
            "DROP TABLE original; INSERT INTO missing VALUES(1);",
        );
        migration(
            &dir,
            "20261014000001",
            "CREATE TABLE failed(at INSTANT); INSERT INTO failed VALUES('invalid');",
            "DROP TABLE failed;",
        );
        migration(
            &dir,
            "20261014000002",
            "CREATE TABLE skipped(body TEXT);",
            "DROP TABLE skipped;",
        );
        assert!(
            !cli(&[
                "migrate",
                "up",
                db.to_str().unwrap(),
                "--dir",
                dir.to_str().unwrap(),
                "--format",
                format
            ])
            .status
            .success()
        );
        assert!(
            !cli(&[
                "migrate",
                "down",
                db.to_str().unwrap(),
                "--dir",
                dir.to_str().unwrap()
            ])
            .status
            .success()
        );
        let output = success(cli(&[
            "migrate",
            "status",
            db.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
            "--json",
        ]));
        let statuses: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(statuses[0]["state"], "applied");
        assert_eq!(statuses[1]["state"], "pending");
        assert_eq!(statuses[2]["state"], "pending");
        success(cli(&[
            db.to_str().unwrap(),
            "-e",
            "INSERT INTO original VALUES('2024-01-01T00:00:00Z');",
        ]));
        assert!(
            !cli(&[db.to_str().unwrap(), "-e", "SELECT * FROM failed;"])
                .status
                .success()
        );
        assert!(
            !cli(&[db.to_str().unwrap(), "-e", "SELECT * FROM skipped;"])
                .status
                .success()
        );
        assert!(
            !cli(&[
                db.to_str().unwrap(),
                "-e",
                "INSERT INTO original VALUES('invalid');"
            ])
            .status
            .success()
        );
    }
}

#[test]
fn concurrent_binary_runners_apply_each_version_exactly_once() {
    use std::process::Stdio;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    let db = temp.path().join("data.db");
    // Create the file first so the two runners contend on migration execution.
    success(cli(&[db.to_str().unwrap(), "-e", "SELECT 1;"]));
    migration(
        &dir,
        "20261014000000",
        "CREATE TABLE events(id INT64 PRIMARY KEY,at INSTANT);",
        "DROP TABLE events;",
    );
    migration(
        &dir,
        "20261014000001",
        "WITH RECURSIVE ids(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<1000) INSERT INTO events SELECT n,'2024-01-01T00:00:00Z' FROM ids;",
        "DELETE FROM events;",
    );
    let spawn = || {
        Command::new(BIN)
            .args([
                "migrate",
                "up",
                db.to_str().unwrap(),
                "--dir",
                dir.to_str().unwrap(),
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn();
    let second = spawn();
    let first: serde_json::Value =
        serde_json::from_str(&success(first.wait_with_output().unwrap())).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&success(second.wait_with_output().unwrap())).unwrap();
    assert_eq!(
        first.as_array().unwrap().len() + second.as_array().unwrap().len(),
        2
    );
    let result: serde_json::Value = serde_json::from_str(&success(cli(&[
        db.to_str().unwrap(),
        "--json",
        "-e",
        "SELECT COUNT(*),SUM(id) FROM events;",
    ])))
    .unwrap();
    assert_eq!(
        result["rows"],
        serde_json::json!([[{"Integer":1000},{"Integer":500500}]])
    );
}
