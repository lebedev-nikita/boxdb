//! A complete application workflow across the CLI and both language clients.
use boxdb::{Database, Format, Value};
use std::{path::Path, process::Command};

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_boxdb"))
}

fn go(root: &Path, path: &Path, phase: &str) {
    let output = run(Command::new("go")
        .current_dir(root.join("packages/go"))
        .args([
            "test",
            "-run",
            "^TestCrossLanguageNativeTypes$",
            "-count=1",
            "-v",
            ".",
        ])
        .env("BOXDB_CLIENT_TYPES_DB", path)
        .env("BOXDB_CLIENT_TYPES_PHASE", phase));
    assert!(
        output.contains("--- PASS: TestCrossLanguageNativeTypes"),
        "{output}"
    );
}

#[test]
#[ignore = "requires built TypeScript and Go clients; run just client-types-test"]
fn native_types_survive_migrations_go_typescript_and_file_conversion() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = tempfile::tempdir().unwrap();
    let migrations = directory.path().join("migrations");
    let source = directory.path().join("source.db");
    let readable = directory.path().join("converted.yaml");
    let restored = directory.path().join("restored.db");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(migrations.join("20261015000000_types.sql"), "-- migrate:up\n\
        CREATE TABLE typed(id INT64 PRIMARY KEY, amount DECIMAL(38,18) NOT NULL, external UUID NOT NULL,\
        occurred INSTANT NOT NULL, day DATE NOT NULL, local DATETIME NOT NULL, duration INTERVAL NOT NULL,\
        document JSON NOT NULL, calculated DECIMAL(38,18) AS (amount+CAST('0.01' AS DECIMAL)) STORED);\
        CREATE UNIQUE INDEX typed_amount ON typed(amount);\
        CREATE UNIQUE INDEX typed_uuid ON typed(external);\
        CREATE UNIQUE INDEX typed_instant ON typed(occurred);\
        CREATE INDEX typed_date ON typed(day);\
        CREATE INDEX typed_datetime ON typed(local);\
        CREATE INDEX typed_interval ON typed(duration);\n\
        -- migrate:down\nDROP TABLE typed;\n").unwrap();
    run(cli()
        .args(["migrate", "up"])
        .arg(&source)
        .arg("--dir")
        .arg(&migrations));
    go(root, &source, "seed");
    run(Command::new("bun")
        .current_dir(root)
        .arg("run")
        .arg("packages/drizzle-adapter/test/cross-language.ts")
        .arg(&source));
    std::fs::write(migrations.join("20261015000001_label.sql"), "-- migrate:up\nALTER TABLE typed ADD COLUMN label VARCHAR(12) NOT NULL DEFAULT 'migrated';\n-- migrate:down\nALTER TABLE typed DROP COLUMN label;\n").unwrap();
    run(cli()
        .args(["migrate", "up"])
        .arg(&source)
        .arg("--dir")
        .arg(&migrations));
    run(cli()
        .arg("convert")
        .arg(&source)
        .arg(&readable)
        .args(["--format", "readable"]));
    run(cli()
        .arg("convert")
        .arg(&readable)
        .arg(&restored)
        .args(["--format", "binary"]));
    go(root, &restored, "verify");
    run(cli()
        .args(["migrate", "up"])
        .arg(&restored)
        .arg("--dir")
        .arg(&migrations));
    run(cli()
        .args(["migrate", "status"])
        .arg(&restored)
        .arg("--dir")
        .arg(&migrations)
        .arg("--check"));
    let mut db = Database::open(&restored, Format::Binary).unwrap();
    for (index, predicate, expected) in [
        ("typed_amount", "amount=CAST('12.500' AS DECIMAL)", vec![2]),
        (
            "typed_uuid",
            "external=CAST('550E8400E29B41D4A716446655440000' AS UUID)",
            vec![1],
        ),
        (
            "typed_instant",
            "occurred=CAST('2024-02-29T16:00:00.123456789+04:00' AS INSTANT)",
            vec![1],
        ),
        (
            "typed_date",
            "day=CAST('2024-02-29' AS DATE)",
            vec![1, 2, 3],
        ),
        (
            "typed_datetime",
            "local=CAST('2024-02-29T12:00:00.123456789' AS DATETIME)",
            vec![1],
        ),
        (
            "typed_interval",
            "duration=CAST('30 days' AS INTERVAL)",
            vec![1, 2],
        ),
    ] {
        let rows = expected
            .into_iter()
            .map(|id| vec![Value::Integer(id)])
            .collect::<Vec<_>>();
        for plan in [format!("INDEXED BY {index}"), "NOT INDEXED".into()] {
            assert_eq!(
                db.execute(&format!(
                    "SELECT id FROM typed {plan} WHERE {predicate} ORDER BY id"
                ))
                .unwrap()[0]
                    .rows,
                rows,
                "{index}: {plan}"
            );
        }
    }
    assert_eq!(
        db.execute("SELECT id FROM typed ORDER BY amount").unwrap()[0].rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)]
        ]
    );
    assert_eq!(
        db.execute("SELECT COUNT(*) FROM typed WHERE label='migrated'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        db.execute("PRAGMA integrity_check").unwrap()[0].rows,
        vec![vec![Value::Text("ok".into())]]
    );
    assert!(
        db.execute("UPDATE typed SET amount='12.5000' WHERE id=3")
            .is_err()
    );
    assert!(
        db.execute("UPDATE typed SET day='2024-02-30' WHERE id=3")
            .is_err()
    );
}
