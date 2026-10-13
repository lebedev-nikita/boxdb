use boxdb::{Database, Value};
use rusqlite::Connection;
mod common;
use common::sqlite_execute;

#[test]
fn native_sql_programs_have_the_same_results_as_sqlite() {
    for program in [
        vec![
            "CREATE TABLE t(id INT64 PRIMARY KEY,n ANY) STRICT",
            "INSERT INTO t VALUES(NULL,'001'),(NULL,9223372036854775807)",
            "SELECT id,n,typeof(n) AS ty FROM t ORDER BY id",
            "UPDATE t SET n=n+1 WHERE id=2 RETURNING id,n",
            "SELECT id,n FROM t",
        ],
        vec![
            "CREATE TABLE t(id INT64 PRIMARY KEY,n TEXT)",
            "INSERT INTO t VALUES(1,'x') ON CONFLICT(id) DO UPDATE SET n=excluded.n",
            "CREATE VIEW v AS SELECT id,n FROM t",
            "CREATE TABLE audit(n TEXT)",
            "CREATE TRIGGER log AFTER UPDATE ON t BEGIN INSERT INTO audit VALUES(NEW.n); INSERT INTO audit VALUES('a;b'); END;",
            "UPDATE t SET n='changed'",
            "SELECT n FROM audit ORDER BY rowid",
            "WITH q AS(SELECT id,n FROM v) SELECT id,n,row_number() OVER(ORDER BY id) AS position FROM q",
        ],
        vec![
            "CREATE TABLE t(n INT64)",
            "INSERT INTO t VALUES(1),(2)",
            "SELECT * FROM t",
            "BEGIN IMMEDIATE",
            "SAVEPOINT s",
            "ALTER TABLE t ADD COLUMN body TEXT DEFAULT 'x'",
            "SELECT * FROM t",
            "ROLLBACK TO s",
            "SELECT * FROM t",
            "COMMIT",
            "SELECT * FROM t WHERE n>0 ORDER BY n",
        ],
        vec![
            "CREATE TABLE parent(id INT64 PRIMARY KEY)",
            "CREATE TABLE child(id INT64 REFERENCES parent(id) ON DELETE CASCADE)",
            "INSERT INTO parent VALUES(1)",
            "INSERT INTO child VALUES(1)",
            "DELETE FROM parent",
            "SELECT id FROM child",
            "PRAGMA foreign_keys",
            "VACUUM",
        ],
        vec![
            "SELECT 0.1+0.2 AS n,1/2 AS fraction,TRUE AS yes,NULL IS NULL AS empty",
            "SELECT json_extract('{\"x\":2}','$.x') AS n",
            "VALUES(1,'雪'),(2,'abc')",
            "CREATE TABLE native_float(n REAL) STRICT",
            "INSERT INTO native_float VALUES(0.1),(1)",
            "SELECT n,typeof(n) FROM native_float",
        ],
    ] {
        let mut boxdb = Database::memory();
        let sqlite = Connection::open_in_memory().unwrap();
        sqlite.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in program {
            assert_eq!(
                boxdb.execute(sql).unwrap(),
                sqlite_execute(&sqlite, sql).unwrap(),
                "{sql}"
            );
        }
    }
}
#[test]
fn parameters_and_statement_atomicity_match_sqlite() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INT64 PRIMARY KEY,n TEXT UNIQUE)")
        .unwrap();
    let params = [Value::Integer(1), Value::Text("'; DROP TABLE t; --".into())];
    db.execute_params("INSERT INTO t VALUES(?1,?2)", &params)
        .unwrap();
    assert_eq!(
        db.execute_params("SELECT n FROM t WHERE id=$1", &[Value::Integer(1)])
            .unwrap()[0]
            .rows,
        vec![vec![params[1].clone()]]
    );
    db.execute_params(
        "UPDATE t SET n=:word WHERE id=:id",
        &[Value::Text("safe".into()), Value::Integer(1)],
    )
    .unwrap();
    db.execute("BEGIN").unwrap();
    assert!(
        db.execute("INSERT INTO t VALUES(2,'new'),(3,'safe')")
            .is_err()
    );
    db.execute("COMMIT").unwrap();
    assert_eq!(
        db.execute("SELECT id,n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1), Value::Text("safe".into())]]
    );
    assert!(db.execute_params("SELECT ?1", &[]).is_err());
    assert!(db.execute_params("SELECT ?1; SELECT ?2", &params).is_err());
}
#[test]
fn cached_queries_follow_schema_changes_even_from_another_client() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shared.db");
    let mut db = Database::open(&path, boxdb::Format::Binary).unwrap();
    db.execute("CREATE TABLE t(n INT64); INSERT INTO t VALUES(1)")
        .unwrap();
    assert_eq!(db.execute("SELECT * FROM t").unwrap()[0].columns, vec!["n"]);
    let sqlite = Connection::open(&path).unwrap();
    sqlite
        .execute_batch("ALTER TABLE t ADD COLUMN body TEXT DEFAULT 'new'")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM t").unwrap()[0].columns,
        vec!["n", "body"]
    );
    sqlite
        .execute_batch("INSERT INTO t VALUES(2,'external')")
        .unwrap();
    assert_eq!(
        db.execute("SELECT * FROM t ORDER BY n").unwrap()[0]
            .rows
            .len(),
        2
    );
}
