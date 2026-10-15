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
fn query(path: &Path, sql: &str) -> serde_json::Value {
    serde_json::from_str(&success(cli(&[
        path.to_str().unwrap(),
        "--json",
        "-e",
        sql,
    ])))
    .unwrap()
}

#[test]
fn default_sql_dump_restores_types_and_objects_without_application_rows() {
    for format in ["binary", "readable"] {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        success(cli(&[
            source.to_str().unwrap(),
            "--format",
            format,
            "-e",
            "CREATE TABLE rich(id INT64 PRIMARY KEY AUTOINCREMENT,b BOOLEAN DEFAULT TRUE,d INSTANT,n DECIMAL(12,2),tags TEXT[],body TEXT CHECK(length(body)>0),twice INT64 GENERATED ALWAYS AS(id*2) VIRTUAL); CREATE TABLE audit(body TEXT); CREATE UNIQUE INDEX unique_body ON rich(body); CREATE VIEW projection AS SELECT b,d,n,tags,twice FROM rich; CREATE TRIGGER record AFTER INSERT ON rich BEGIN INSERT INTO audit VALUES(NEW.body); END; INSERT INTO rich(id,body) VALUES(100,'private-row-must-not-be-exported');",
        ]));
        let before = std::fs::read(&source).unwrap();
        let dump = success(cli(&["schema", "dump", source.to_str().unwrap()]));
        assert_eq!(
            dump,
            success(cli(&[
                "schema",
                "dump",
                source.to_str().unwrap(),
                "--format=sql"
            ]))
        );
        assert!(!dump.contains("private-row-must-not-be-exported"));
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let restored = temp.path().join("restored.db");
        let sqlite = rusqlite::Connection::open(&restored).unwrap();
        sqlite.execute_batch(&dump).unwrap();
        drop(sqlite);
        assert_eq!(
            query(&restored, "SELECT COUNT(*) FROM rich;")["rows"][0][0]["Integer"],
            0
        );
        success(cli(&[
            restored.to_str().unwrap(),
            "-e",
            "INSERT INTO rich(d,n,tags,body) VALUES('2024-01-01T00:00:00Z','12.50','[\"a\"]','restored');",
        ]));
        assert_eq!(
            query(&restored, "SELECT b,d,n,tags,twice FROM projection;")["rows"],
            serde_json::json!([[{"Boolean":true},{"Instant":"2024-01-01T00:00:00.000000Z"},{"Decimal":"12.50"},{"Array":[{"Text":"a"}]},{"Integer":2}]])
        );
        assert_eq!(
            query(&restored, "SELECT body FROM audit;")["rows"][0][0]["Text"],
            "restored"
        );
        assert!(
            !cli(&[
                restored.to_str().unwrap(),
                "-e",
                "INSERT INTO rich(body) VALUES('restored');"
            ])
            .status
            .success()
        );
        assert!(
            !cli(&[
                restored.to_str().unwrap(),
                "-e",
                "INSERT INTO rich(d,body) VALUES('invalid','other');"
            ])
            .status
            .success()
        );
    }
}

#[test]
fn yaml_dump_is_a_readable_schema_snapshot_that_restores_through_convert() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE \"odd' table\"(id INT64 PRIMARY KEY AUTOINCREMENT,\"odd' column\" INSTANT,n DECIMAL(12,2),tags UINT8[],b BOOLEAN); INSERT INTO \"odd' table\" VALUES(100,'2024-01-01T00:00:00Z','12.50','[255]',1);",
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    assert!(dump.starts_with("---\nkind: database\n"));
    let yaml = temp.path().join("schema.yaml");
    std::fs::write(&yaml, dump).unwrap();
    let restored = temp.path().join("restored.db");
    success(cli(&[
        "convert",
        yaml.to_str().unwrap(),
        restored.to_str().unwrap(),
        "--format",
        "binary",
    ]));
    assert_eq!(
        query(&restored, "SELECT COUNT(*) FROM \"odd' table\";")["rows"][0][0]["Integer"],
        0
    );
    success(cli(&[
        restored.to_str().unwrap(),
        "-e",
        "INSERT INTO \"odd' table\"(\"odd' column\",n,tags,b) VALUES('2024-01-01T00:00:00Z','12.50','[255]',1);",
    ]));
    assert_eq!(
        query(
            &restored,
            "SELECT id,\"odd' column\",n,tags,b FROM \"odd' table\";"
        )["rows"],
        serde_json::json!([[{"Integer":1},{"Instant":"2024-01-01T00:00:00.000000Z"},{"Decimal":"12.50"},{"Array":[{"UnsignedInteger":255}]},{"Boolean":true}]])
    );
    assert!(
        !cli(&[
            restored.to_str().unwrap(),
            "-e",
            "INSERT INTO \"odd' table\"(tags) VALUES('[256]');"
        ])
        .status
        .success()
    );
}

#[test]
fn schema_restoration_preserves_applied_versions_so_up_can_continue() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("migrations");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("20261014000000_events.sql"),
        "-- migrate:up\nCREATE TABLE events(at INSTANT);\n-- migrate:down\nDROP TABLE events;\n",
    )
    .unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        "migrate",
        "up",
        source.to_str().unwrap(),
        "--dir",
        dir.to_str().unwrap(),
    ]));
    for format in ["sql", "yaml"] {
        let restored = temp.path().join(format!("restored-{format}.db"));
        let dump = success(cli(&[
            "schema",
            "dump",
            source.to_str().unwrap(),
            "--format",
            format,
        ]));
        if format == "sql" {
            rusqlite::Connection::open(&restored)
                .unwrap()
                .execute_batch(&dump)
                .unwrap();
        } else {
            let yaml = temp.path().join("schema.yaml");
            std::fs::write(&yaml, dump).unwrap();
            success(cli(&[
                "convert",
                yaml.to_str().unwrap(),
                restored.to_str().unwrap(),
                "--format",
                "binary",
            ]));
        }
        success(cli(&[
            "migrate",
            "status",
            restored.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
            "--check",
        ]));
        assert_eq!(
            success(cli(&[
                "migrate",
                "up",
                restored.to_str().unwrap(),
                "--dir",
                dir.to_str().unwrap(),
                "--json"
            ]))
            .trim(),
            "[]"
        );
        let second = dir.join("20261014000001_tags.sql");
        std::fs::write(&second, "-- migrate:up\nALTER TABLE events ADD COLUMN tags TEXT[];\n-- migrate:down\nALTER TABLE events DROP COLUMN tags;\n").unwrap();
        success(cli(&[
            "migrate",
            "up",
            restored.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ]));
        success(cli(&[
            restored.to_str().unwrap(),
            "-e",
            "INSERT INTO events VALUES('2024-01-01T00:00:00Z','[\"restored\"]');",
        ]));
        assert_eq!(
            query(&restored, "SELECT tags FROM events;")["rows"][0][0],
            serde_json::json!({"Array":[{"Text":"restored"}]})
        );
        std::fs::remove_file(second).unwrap();
    }
}

#[test]
fn user_tables_with_names_similar_to_sqlite_system_tables_are_exported() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE sqlitex_records(at INSTANT);",
    ]));
    for format in ["sql", "yaml"] {
        let restored = temp.path().join(format!("restored-{format}.db"));
        let dump = success(cli(&[
            "schema",
            "dump",
            source.to_str().unwrap(),
            "--format",
            format,
        ]));
        if format == "sql" {
            rusqlite::Connection::open(&restored)
                .unwrap()
                .execute_batch(&dump)
                .unwrap();
        } else {
            let yaml = temp.path().join("schema.yaml");
            std::fs::write(&yaml, dump).unwrap();
            success(cli(&[
                "convert",
                yaml.to_str().unwrap(),
                restored.to_str().unwrap(),
                "--format",
                "binary",
            ]));
        }
        success(cli(&[
            restored.to_str().unwrap(),
            "-e",
            "INSERT INTO sqlitex_records VALUES('2024-01-01T00:00:00Z');",
        ]));
        assert_eq!(
            query(&restored, "SELECT at FROM sqlitex_records;")["rows"][0][0]["Instant"],
            "2024-01-01T00:00:00.000000Z"
        );
    }
}
