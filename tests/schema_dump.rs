use std::{
    path::Path,
    process::{Command, Output},
};
const BIN: &str = env!("CARGO_BIN_EXE_boxdb");

#[test]
fn yaml_column_keys_mark_nullable_columns_instead_of_sql_null_clauses() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY, email TEXT NOT NULL, nickname TEXT NULL, active BOOLEAN NOT NULL DEFAULT TRUE, tags TEXT[], \"odd: name\" TEXT DEFAULT 'NOT NULL', derived TEXT AS (coalesce(nickname, 'NULL'))); CREATE TABLE composite(a TEXT, b INT64, PRIMARY KEY(a,b)) WITHOUT ROWID",
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["tables"]["accounts"]["columns"],
        serde_json::json!({
            "id": "INTEGER PRIMARY KEY",
            "email": "TEXT",
            "nickname?": "TEXT",
            "active": "BOOLEAN DEFAULT TRUE",
            "tags?": "TEXT[]",
            "odd: name?": "TEXT DEFAULT 'NOT NULL'",
            "derived?": "TEXT AS (coalesce(nickname, 'NULL'))",
        })
    );
    assert_eq!(
        document["tables"]["composite"]["columns"],
        serde_json::json!({
            "a": "TEXT", "b": "INTEGER",
        })
    );
}

#[test]
fn yaml_nullability_keeps_named_constraints_conflicts_defaults_and_foreign_keys() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(named TEXT CONSTRAINT required NOT NULL DEFAULT 'NULL', policy TEXT NOT NULL ON CONFLICT FAIL, optional TEXT DEFAULT NULL REFERENCES parent(id) ON DELETE SET NULL CHECK(optional IS NULL), tags TEXT[] NOT NULL DEFAULT '[]')",
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["tables"]["child"]["columns"],
        serde_json::json!({
            "named": "TEXT CONSTRAINT required NOT NULL DEFAULT 'NULL'",
            "policy": "TEXT NOT NULL ON CONFLICT FAIL",
            "optional?": "TEXT DEFAULT NULL REFERENCES parent(id) ON DELETE SET NULL CHECK (optional IS NULL)",
            "tags": "TEXT[] DEFAULT '[]'",
        })
    );
}

#[test]
fn yaml_dump_preserves_enum_labels_arrays_and_user_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let mut canonical = None;
    for format in ["binary", "readable"] {
        let source = temp.path().join(format!("source-{format}"));
        success(cli(&[
            source.to_str().unwrap(),
            "--format",
            format,
            "-e",
            r#"
            CREATE TABLE tasks(
                state ENUM('todo','Done','it''s, (雪)[] ARRAY<','') NOT NULL DEFAULT 'todo'
                    CONSTRAINT user_state CHECK(state <> ''),
                labels ENUM('a','it''s, Done')[],
                nested ENUM('x','Y')[][],
                derived ENUM('todo','Done','it''s, (雪)[] ARRAY<','') AS(state) STORED
            );
            ALTER TABLE tasks RENAME COLUMN state TO status;
            ALTER TABLE tasks ADD COLUMN priority ENUM('low','high') DEFAULT 'low';
        "#,
        ]));
        let dump = success(cli(&[
            "schema",
            "dump",
            source.to_str().unwrap(),
            "--format=yaml",
        ]));
        let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
        let columns = &document["tables"]["tasks"]["columns"];
        assert_eq!(
            columns["status"],
            "ENUM('todo', 'Done', 'it''s, (雪)[] ARRAY<', '') DEFAULT 'todo' CONSTRAINT user_state CHECK (status <> '')"
        );
        assert_eq!(columns["labels?"], "ENUM('a', 'it''s, Done')[]");
        assert_eq!(columns["nested?"], "ENUM('x', 'Y')[][]");
        assert_eq!(
            columns["derived?"],
            "ENUM('todo', 'Done', 'it''s, (雪)[] ARRAY<', '') AS (status) STORED"
        );
        assert_eq!(columns["priority?"], "ENUM('low', 'high') DEFAULT 'low'");
        assert!(!dump.contains("__boxdb_"));
        assert!(!dump.contains("typeof"));
        if let Some(expected) = &canonical {
            assert_eq!(&dump, expected);
        } else {
            canonical = Some(dump);
        }
    }
}

#[test]
fn yaml_schema_documents_are_export_only() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE items(id INTEGER PRIMARY KEY)",
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let yaml = temp.path().join("schema.yaml");
    std::fs::write(&yaml, &dump).unwrap();
    for format in ["binary", "readable"] {
        let destination = temp.path().join(format!("destination-{format}"));
        let result = cli(&[
            "convert",
            yaml.to_str().unwrap(),
            destination.to_str().unwrap(),
            "--format",
            format,
        ]);
        assert!(
            !result.status.success(),
            "schema dump was accepted as a database"
        );
        assert!(!destination.exists());
    }
    assert!(
        !cli(&[yaml.to_str().unwrap(), "-e", "SELECT 1"])
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(yaml).unwrap(), dump);
}

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
fn yaml_dump_has_named_logical_columns() {
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
    assert!(dump.starts_with("format: boxdb-schema\nversion: 1\n"));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["tables"]["odd' table"]["columns"],
        serde_json::json!({
            "id": "INTEGER PRIMARY KEY AUTOINCREMENT",
            "odd' column?": "INSTANT",
            "n?": "DECIMAL(12, 2)",
            "tags?": "UINT8[]",
            "b?": "BOOLEAN",
        })
    );
    assert!(!dump.contains("__boxdb_"));
    assert!(!dump.contains("strict:"));
    assert!(dump.find("      id:").unwrap() < dump.find("      odd' column?:").unwrap());
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
    let yaml = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
    assert_eq!(document["migrations"][0]["version"], "20261014000000");
    assert_eq!(document["migrations"][0]["name"], "events");
    assert_eq!(
        document["migrations"][0]["checksum"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let restored = temp.path().join("restored.db");
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=sql",
    ]));
    rusqlite::Connection::open(&restored)
        .unwrap()
        .execute_batch(&dump)
        .unwrap();
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

#[test]
fn user_tables_with_names_similar_to_sqlite_system_tables_are_exported() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        "CREATE TABLE sqlitex_records(at INSTANT);",
    ]));
    let yaml = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
    assert_eq!(
        document["tables"]["sqlitex_records"]["columns"]["at?"],
        "INSTANT"
    );
    let restored = temp.path().join("restored.db");
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=sql",
    ]));
    rusqlite::Connection::open(&restored)
        .unwrap()
        .execute_batch(&dump)
        .unwrap();
    success(cli(&[
        restored.to_str().unwrap(),
        "-e",
        "INSERT INTO sqlitex_records VALUES('2024-01-01T00:00:00Z')",
    ]));
    assert_eq!(
        query(&restored, "SELECT at FROM sqlitex_records")["rows"][0][0]["Instant"],
        "2024-01-01T00:00:00.000000Z"
    );
}

#[test]
fn yaml_constraints_preserve_names_keys_checks_and_deferred_foreign_keys() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        r#"
        CREATE TABLE parents(a TEXT,b INTEGER,CONSTRAINT parent_key PRIMARY KEY(a,b)) WITHOUT ROWID;
        CREATE TABLE children(z INTEGER,a TEXT,b INTEGER,flag BOOLEAN CHECK(flag IN(0,1)),
          CONSTRAINT child_unique UNIQUE(a,b) ON CONFLICT IGNORE,
          CONSTRAINT positive CHECK(z>0),
          CONSTRAINT parent_fk FOREIGN KEY(a,b) REFERENCES parents(a,b)
            ON DELETE CASCADE ON UPDATE SET NULL DEFERRABLE INITIALLY DEFERRED);
    "#,
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(document["tables"]["parents"]["without_rowid"], true);
    assert_eq!(
        document["tables"]["parents"]["constraints"][0],
        serde_json::json!({
            "name": "parent_key", "primary_key": ["a", "b"]
        })
    );
    assert_eq!(
        document["tables"]["children"]["constraints"][0],
        serde_json::json!({
            "name": "child_unique", "unique": ["a", "b"], "on_conflict": "ignore"
        })
    );
    assert_eq!(
        document["tables"]["children"]["constraints"][2],
        serde_json::json!({
            "name": "parent_fk", "foreign_key": ["a", "b"],
            "references": {"table": "parents", "columns": ["a", "b"]},
            "on_delete": "cascade", "on_update": "set_null", "deferrable": true, "initially": "deferred"
        })
    );
}

#[test]
fn yaml_indexes_views_and_triggers_have_structured_fields() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        r#"
        CREATE TABLE accounts(id INTEGER PRIMARY KEY,email TEXT,active BOOLEAN DEFAULT TRUE,begin BOOLEAN DEFAULT TRUE,
          label TEXT AS (text_lower(email)) STORED);
        CREATE TABLE audit(message TEXT);
        CREATE UNIQUE INDEX active_email ON accounts(text_lower(email) COLLATE text_nocase DESC,id ASC) WHERE active=TRUE;
        CREATE INDEX account_label ON accounts(label);
        CREATE VIEW active_accounts(account_id,email) AS SELECT id,email FROM accounts WHERE active=TRUE;
        CREATE TRIGGER account_changed AFTER UPDATE OF email,active ON accounts FOR EACH ROW
          WHEN NEW.begin AND (OLD.email IS NOT NEW.email OR OLD.active IS NOT NEW.active) BEGIN
          INSERT INTO audit VALUES(CASE WHEN NEW.active THEN 'active; END' ELSE 'inactive' END);
          INSERT INTO audit VALUES(NEW.email);
        END;
        CREATE TRIGGER delete_active INSTEAD OF DELETE ON active_accounts BEGIN
          UPDATE accounts SET active=FALSE WHERE id=OLD.account_id;
        END;
    "#,
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["indexes"]["active_email"],
        serde_json::json!({
            "type": "unique", "table": "accounts", "expressions": ["text_lower(email) COLLATE text_nocase DESC", "id ASC"], "where": "active = TRUE"
        })
    );
    assert_eq!(document["indexes"]["account_label"]["type"], "regular");
    assert_eq!(
        document["views"]["active_accounts"]["columns"],
        serde_json::json!(["account_id", "email"])
    );
    assert_eq!(
        document["triggers"]["account_changed"]["update_of"],
        serde_json::json!(["email", "active"])
    );
    assert_eq!(
        document["triggers"]["account_changed"]["when"],
        "NEW.begin AND (OLD.email IS NOT NEW.email OR OLD.active IS NOT NEW.active)"
    );
    assert_eq!(
        document["triggers"]["delete_active"]["timing"],
        "instead_of"
    );
    assert!(dump.contains("query: |"));
    assert!(dump.contains("body: |"));
    assert_eq!(
        document["views"]["active_accounts"]["query"],
        "SELECT id, email\nFROM accounts\nWHERE active = TRUE\n"
    );
    assert_eq!(
        document["triggers"]["account_changed"]["body"],
        "INSERT INTO audit VALUES (CASE WHEN NEW.active THEN 'active; END' ELSE 'inactive' END);\nINSERT INTO audit VALUES (NEW.email);\n"
    );
}

#[test]
fn yaml_constraint_keys_keep_collation_sort_order_and_quoted_names() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    success(cli(&[
        source.to_str().unwrap(),
        "-e",
        r#"
        CREATE TABLE "odd: table"("z key" TEXT,"a'key" INTEGER,
          CONSTRAINT "unique: key" UNIQUE("z key" COLLATE text_nocase DESC,"a'key" ASC)
          ON CONFLICT REPLACE);
        CREATE TABLE counter(id INTEGER,CONSTRAINT counter_pk PRIMARY KEY(id DESC AUTOINCREMENT));
    "#,
    ]));
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["tables"]["odd: table"]["constraints"][0]["unique"],
        serde_json::json!([
            {"column":"z key","collate":"text_nocase","order":"desc"}, {"column":"a'key","order":"asc"}
        ])
    );
    assert_eq!(
        document["tables"]["counter"]["constraints"][0]["autoincrement"],
        true
    );
}

#[test]
fn yaml_dump_rejects_non_strict_tables_instead_of_changing_their_contract() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("stock.db");
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute_batch("CREATE TABLE legacy(value TEXT)").unwrap();
    drop(db);
    let result = cli(&["schema", "dump", source.to_str().unwrap(), "--format=yaml"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("non-strict"));
    assert!(
        success(cli(&[
            "schema",
            "dump",
            source.to_str().unwrap(),
            "--format=sql"
        ]))
        .contains("CREATE TABLE legacy")
    );
}

#[test]
fn yaml_dump_rejects_external_column_names_with_reserved_question_mark_suffix() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("external.db");
    rusqlite::Connection::open(&source)
        .unwrap()
        .execute_batch("CREATE TABLE external(\"value?\" TEXT) STRICT")
        .unwrap();
    let result = cli(&["schema", "dump", source.to_str().unwrap(), "--format=yaml"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("column names cannot end with '?'"));
    assert!(result.stdout.is_empty());
}

#[test]
fn yaml_dump_preserves_typed_columns_after_rename_and_add_column() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.yaml");
    success(cli(&[
        source.to_str().unwrap(),
        "--format=readable",
        "-e",
        r#"
        CREATE TABLE before("old name" BOOLEAN CONSTRAINT user_check CHECK("old name" IN(0,1)), n DECIMAL(9,2));
        ALTER TABLE before RENAME COLUMN "old name" TO "new name";
        ALTER TABLE before RENAME TO after;
        ALTER TABLE after ADD COLUMN code VARCHAR(5) DEFAULT 'a';
    "#,
    ]));
    let before = std::fs::read(&source).unwrap();
    let dump = success(cli(&[
        "schema",
        "dump",
        source.to_str().unwrap(),
        "--format=yaml",
    ]));
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert!(!dump.contains("__boxdb_"));
    assert!(dump.contains("CONSTRAINT user_check CHECK"));
    let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
    assert_eq!(
        document["tables"]["after"]["columns"]["n?"],
        "DECIMAL(9, 2)"
    );
    assert_eq!(
        document["tables"]["after"]["columns"]["code?"],
        "VARCHAR(5) DEFAULT 'a'"
    );
    assert!(
        document["tables"]["after"]["columns"]["new name?"]
            .as_str()
            .unwrap()
            .starts_with("BOOLEAN ")
    );
}

#[test]
fn yaml_dump_matches_the_example_and_is_independent_of_storage_and_data() {
    let temp = tempfile::tempdir().unwrap();
    let mut expected: serde_json::Value =
        serde_saphyr::from_str(include_str!("fixtures/schema.yaml")).unwrap();
    expected.as_object_mut().unwrap().remove("migrations");
    let mut canonical = None;
    for format in ["binary", "readable"] {
        let source = temp.path().join(format!("source-{format}"));
        success(cli(&[
            source.to_str().unwrap(),
            "--format",
            format,
            "-e",
            include_str!("fixtures/schema.sql"),
        ]));
        let dump = success(cli(&[
            "schema",
            "dump",
            source.to_str().unwrap(),
            "--format=yaml",
        ]));
        let document: serde_json::Value = serde_saphyr::from_str(&dump).unwrap();
        assert_eq!(document, expected);
        if let Some(expected) = &canonical {
            assert_eq!(&dump, expected);
        } else {
            canonical = Some(dump.clone());
        }
        success(cli(&[
            source.to_str().unwrap(),
            "-e",
            "INSERT INTO samples(id,label) VALUES(100,'private data'); UPDATE samples SET label='changed data'",
        ]));
        let before = std::fs::read(&source).unwrap();
        assert_eq!(
            dump,
            success(cli(&[
                "schema",
                "dump",
                source.to_str().unwrap(),
                "--format=yaml"
            ]))
        );
        assert_eq!(std::fs::read(source).unwrap(), before);
    }
}
