use boxdb::{Database, Format, Value};
use std::process::Command;
fn run(args: &[&str]) {
    let output = Command::new("litestream")
        .args(args)
        .output()
        .expect("install original Litestream to run this integration test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "requires the original Litestream executable"]
fn original_litestream_replicates_wal_and_restores_typed_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.db");
    let restored = temp.path().join("restored.db");
    let replica = format!("file://{}", temp.path().join("replica").display());
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY,d DATE); INSERT INTO t VALUES(1,'2024-01-01')",
    )
    .unwrap();
    run(&["replicate", "-once", path.to_str().unwrap(), &replica]);
    db.execute("INSERT INTO t VALUES(2,'2024-02-29')").unwrap();
    run(&["replicate", "-once", path.to_str().unwrap(), &replica]);
    run(&["restore", "-o", restored.to_str().unwrap(), &replica]);
    let mut restored = Database::open(restored, Format::Binary).unwrap();
    assert_eq!(
        restored.execute("SELECT id,d FROM t ORDER BY id").unwrap()[0].rows,
        vec![
            vec![Value::Integer(1), Value::Date("2024-01-01".into())],
            vec![Value::Integer(2), Value::Date("2024-02-29".into())]
        ]
    );
    assert!(
        restored
            .execute("INSERT INTO t VALUES(3,'invalid')")
            .is_err()
    );
}
