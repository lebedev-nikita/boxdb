use boxdb::{Database, Value};

#[test]
fn query_validation_does_not_depend_on_existing_rows() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE a(id INTEGER); CREATE TABLE b(id INTEGER)")
        .unwrap();
    for sql in [
        "SELECT id FROM a ORDER BY missing",
        "SELECT id FROM a ORDER BY 2",
        "SELECT * FROM a JOIN b ON a.missing=b.id",
        "SELECT * FROM a NATURAL JOIN b",
        "SELECT * FROM a JOIN b USING(id)",
    ] {
        assert!(db.execute(sql).is_err(), "{sql}");
    }
    assert!(db.execute("VALUES (1),(2,3)").is_err());
    assert!(db.execute("SELECT id AS x FROM a ORDER BY x").is_ok());
}

#[test]
fn typed_rows_can_be_inserted_and_selected() {
    let mut db = Database::memory();
    let result = db.execute("CREATE TABLE people (id INTEGER, name TEXT); INSERT INTO people VALUES (1, 'Ada'); SELECT name, id FROM people;").unwrap();
    assert_eq!(
        result.last().unwrap().rows,
        vec![vec![Value::Text("Ada".into()), Value::Integer(1)]]
    );
}

#[test]
fn parameters_preserve_types_and_cannot_inject_sql() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER,name TEXT,n NUMERIC,tags TEXT[])")
        .unwrap();
    let name = "'); DROP TABLE t; --";
    db.execute_params(
        "INSERT INTO t VALUES(?1,?2,?3,?4)",
        &[
            Value::Integer(1),
            Value::Text(name.into()),
            Value::Numeric("0.1".into()),
            Value::Array(vec![Value::Text("a".into())]),
        ],
    )
    .unwrap();
    assert_eq!(
        db.execute_params("SELECT name,n FROM t WHERE id=$1", &[Value::Integer(1)])
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text(name.into()), Value::Numeric("0.1".into())]]
    );
    assert!(
        db.execute_params("INSERT INTO t(id) VALUES(?)", &[Value::Text("1".into())])
            .is_err()
    );
    assert!(db.execute_params("SELECT ?", &[]).is_err());
    assert!(db.execute_params("SELECT 1", &[Value::Integer(1)]).is_err());
}

#[test]
fn arrays_compare_by_value_and_timestamp_order_includes_fractional_seconds() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(tags TEXT[],ts TIMESTAMPTZ); INSERT INTO t VALUES(ARRAY['a'],CAST('2024-01-01T00:00:00.100Z' AS TIMESTAMPTZ)),(ARRAY['b'],CAST('2024-01-01T00:00:00Z' AS TIMESTAMPTZ))").unwrap();
    assert_eq!(
        db.execute("SELECT tags FROM t WHERE tags=ARRAY['a']")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Array(vec![Value::Text("a".into())])]]
    );
    assert_eq!(
        db.execute("SELECT ts FROM t ORDER BY ts").unwrap()[0].rows,
        vec![
            vec![Value::Timestamptz("2024-01-01T00:00:00Z".into())],
            vec![Value::Timestamptz("2024-01-01T00:00:00.100Z".into())]
        ]
    );
}

#[test]
fn foreign_keys_match_numeric_values_and_permuted_unique_keys() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE parent(n NUMERIC(12,2) PRIMARY KEY); CREATE TABLE child(n NUMERIC REFERENCES parent(n) ON UPDATE CASCADE); INSERT INTO parent VALUES(1.20); INSERT INTO child VALUES(1.2); UPDATE parent SET n=2.25").unwrap();
    assert_eq!(
        db.execute("SELECT n FROM child").unwrap()[0].rows,
        vec![vec![Value::Numeric("2.25".into())]]
    );
    db.execute("CREATE TABLE pair(a INTEGER,b TEXT,UNIQUE(a,b)); CREATE TABLE link(x TEXT,y INTEGER,FOREIGN KEY(x,y) REFERENCES pair(b,a)); INSERT INTO pair VALUES(1,'a'); INSERT INTO link VALUES('a',1)").unwrap();
}

#[test]
fn aggregates_preserve_rich_types_and_decimal_averages_are_exact() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(d DATE,n NUMERIC); INSERT INTO t VALUES(DATE '2024-01-01',0.1),(DATE '2024-02-01',0.2)").unwrap();
    assert_eq!(
        db.execute("SELECT min(d),max(d),avg(n),sum(n) FROM t")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Date("2024-01-01".into()),
            Value::Date("2024-02-01".into()),
            Value::Numeric("0.15".into()),
            Value::Numeric("0.3".into())
        ]]
    );
}

#[test]
fn invalid_schemas_and_queries_fail_even_when_tables_are_empty() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER)").unwrap();
    for sql in [
        "CREATE TABLE bad(n INTEGER CHECK(missing>0))",
        "CREATE TABLE child(n INTEGER REFERENCES t(n))",
        "UPDATE t SET missing=1",
        "UPDATE t SET n=missing",
        "DELETE FROM t WHERE missing=1",
        "SELECT n FROM t WHERE missing=1",
        "SELECT CASE WHEN TRUE THEN 1 ELSE missing END FROM t",
        "INSERT INTO t(n,n) VALUES(1,2)",
        "SELECT CAST('1e5000' AS NUMERIC)",
    ] {
        assert!(db.execute(sql).is_err(), "accepted: {sql}");
    }
}

#[test]
fn default_values_and_insert_returning_generate_integer_primary_keys() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n TEXT DEFAULT 'default')")
        .unwrap();
    assert_eq!(
        db.execute("INSERT INTO t DEFAULT VALUES RETURNING id,n")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1), Value::Text("default".into())]]
    );
    assert_eq!(
        db.execute("INSERT INTO t(n) VALUES('Ada'),('Bob') RETURNING id")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]
    );
    assert_eq!(
        db.execute("INSERT INTO t VALUES(NULL,'Cara') RETURNING id")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(4)]]
    );
}

#[test]
fn unsupported_clauses_fail_instead_of_silently_changing_meaning() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    for sql in [
        "INSERT INTO t VALUES(2) ON CONFLICT DO NOTHING",
        "INSERT OR IGNORE INTO t VALUES(2)",
        "CREATE TEMPORARY TABLE hidden(n INTEGER)",
        "SELECT n FROM t FOR UPDATE",
        "WITH ignored AS (SELECT 2) SELECT n FROM t",
        "SELECT count(*) FILTER(WHERE n<0) FROM t",
        "SELECT DISTINCT ON(n) n FROM t",
    ] {
        assert!(db.execute(sql).is_err(), "accepted: {sql}");
    }
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn foreign_key_actions_cascade_updates_and_deletes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER PRIMARY KEY,p INTEGER REFERENCES parent ON UPDATE CASCADE ON DELETE CASCADE); CREATE TABLE optional(p INTEGER REFERENCES parent(id) ON UPDATE CASCADE ON DELETE SET NULL); INSERT INTO parent VALUES(1); INSERT INTO child VALUES(2,1); INSERT INTO optional VALUES(1); UPDATE parent SET id=3").unwrap();
    assert_eq!(
        db.execute("SELECT p FROM child").unwrap()[0].rows,
        vec![vec![Value::Integer(3)]]
    );
    db.execute("DELETE FROM parent").unwrap();
    assert!(
        db.execute("SELECT * FROM child").unwrap()[0]
            .rows
            .is_empty()
    );
    assert_eq!(
        db.execute("SELECT p FROM optional").unwrap()[0].rows,
        vec![vec![Value::Null]]
    );
}

#[test]
fn indexes_enforce_unique_values_and_have_a_queryable_catalogue() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1),(NULL),(NULL); CREATE UNIQUE INDEX t_n ON t(n)").unwrap();
    assert!(db.execute("INSERT INTO t VALUES(1)").is_err());
    assert!(db.execute("CREATE INDEX bad ON t(missing)").is_err());
    assert_eq!(
        db.execute("SELECT name FROM sqlite_schema WHERE type='index'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("t_n".into())]]
    );
    db.execute("DROP INDEX t_n; INSERT INTO t VALUES(1)")
        .unwrap();
    assert_eq!(
        db.execute("SELECT count(*) FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(4)]]
    );
}

#[test]
fn transactions_and_savepoints_rollback_schema_and_rows() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1); BEGIN; UPDATE t SET n=2; SAVEPOINT middle; INSERT INTO t VALUES(3); ROLLBACK TO middle; RELEASE middle; COMMIT").unwrap();
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
    db.execute("BEGIN; CREATE TABLE temporary(n INTEGER); INSERT INTO t VALUES(4); ROLLBACK")
        .unwrap();
    assert!(db.execute("SELECT * FROM temporary").is_err());
    assert_eq!(
        db.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
    assert!(db.execute("COMMIT").is_err());
    db.execute("BEGIN").unwrap();
    assert!(db.execute("BEGIN").is_err());
    assert!(db.execute("INSERT INTO t VALUES('bad')").is_err());
    db.execute("INSERT INTO t VALUES(5); COMMIT").unwrap();
    assert_eq!(db.execute("SELECT n FROM t").unwrap()[0].rows.len(), 2);
}

#[test]
fn aggregates_group_rows_ignore_null_and_compound_queries_remove_duplicates() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE stats(k TEXT,n INTEGER); INSERT INTO stats VALUES('a',1),('a',3),('b',NULL)",
    )
    .unwrap();
    assert_eq!(
        db.execute("SELECT count(*),count(n),sum(n),avg(n),min(n),max(n) FROM stats")
            .unwrap()[0]
            .rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(2),
            Value::Integer(4),
            Value::Real(2.0),
            Value::Integer(1),
            Value::Integer(3)
        ]]
    );
    assert_eq!(
        db.execute(
            "SELECT k,sum(n) AS total FROM stats GROUP BY k HAVING count(n)>0 ORDER BY total"
        )
        .unwrap()[0]
            .rows,
        vec![vec![Value::Text("a".into()), Value::Integer(4)]]
    );
    assert_eq!(
        db.execute("SELECT sum(n),total(n),count(*) FROM stats WHERE n<0")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Null, Value::Real(0.0), Value::Integer(0)]]
    );
    assert_eq!(
        db.execute("SELECT DISTINCT k FROM stats ORDER BY k")
            .unwrap()[0]
            .rows
            .len(),
        2
    );
    assert_eq!(
        db.execute("SELECT n FROM stats UNION SELECT 1 ORDER BY n")
            .unwrap()[0]
            .rows,
        vec![
            vec![Value::Null],
            vec![Value::Integer(1)],
            vec![Value::Integer(3)]
        ]
    );
}

#[test]
fn joins_resolve_qualified_columns_and_preserve_unmatched_left_rows() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE people(id INTEGER,name TEXT); CREATE TABLE visits(person INTEGER); INSERT INTO people VALUES(1,'Ada'),(2,'Bob'); INSERT INTO visits VALUES(1),(1)").unwrap();
    assert_eq!(db.execute("SELECT p.name,v.person FROM people AS p LEFT JOIN visits AS v ON p.id=v.person ORDER BY p.id").unwrap()[0].rows,vec![vec![Value::Text("Ada".into()),Value::Integer(1)],vec![Value::Text("Ada".into()),Value::Integer(1)],vec![Value::Text("Bob".into()),Value::Null]]);
    assert_eq!(
        db.execute("SELECT p.id FROM people p,visits v WHERE p.id=v.person")
            .unwrap()[0]
            .rows
            .len(),
        2
    );
    assert!(db.execute("SELECT id FROM people p,people q").is_err());
}

#[test]
fn updates_read_old_row_values_and_deletes_enforce_references() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE pairs(a INTEGER, b INTEGER); INSERT INTO pairs VALUES(1,10),(2,20),(3,30)",
    )
    .unwrap();
    let r = db
        .execute("UPDATE pairs SET a=b,b=a WHERE a>=2 RETURNING a,b")
        .unwrap();
    assert_eq!(r[0].affected, 2);
    assert_eq!(
        r[0].rows,
        vec![
            vec![Value::Integer(20), Value::Integer(2)],
            vec![Value::Integer(30), Value::Integer(3)]
        ]
    );
    assert!(db.execute("UPDATE pairs SET a='wrong'").is_err());
    assert_eq!(
        db.execute("SELECT b AS old FROM pairs WHERE a>1 ORDER BY a DESC LIMIT 1")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        db.execute("DELETE FROM pairs WHERE a=20 RETURNING b")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    db.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY); INSERT INTO parent VALUES(1); CREATE TABLE child(id INTEGER REFERENCES parent(id)); INSERT INTO child VALUES(1)").unwrap();
    assert!(db.execute("DELETE FROM parent").is_err());
    assert_eq!(
        db.execute("SELECT id FROM parent").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn constraints_and_defaults_are_enforced_atomically() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE teams (id BIGINT PRIMARY KEY); INSERT INTO teams VALUES(1); CREATE TABLE users (id BIGINT PRIMARY KEY, team BIGINT REFERENCES teams(id), name TEXT NOT NULL UNIQUE, age SMALLINT DEFAULT 18 CHECK(age>=0)); INSERT INTO users(id,team,name) VALUES(1,1,'Ada')").unwrap();
    for sql in [
        "INSERT INTO users(id,name) VALUES(1,'Bob')",
        "INSERT INTO users(id,name) VALUES(2,'Ada')",
        "INSERT INTO users(id,name,age) VALUES(2,'Bob',-1)",
        "INSERT INTO users(id,name,team) VALUES(2,'Bob',99)",
        "INSERT INTO users(id) VALUES(2)",
        "INSERT INTO users(id,name) VALUES(2,'Bob'),(3,'Ada')",
        "CREATE TABLE users (other TEXT)",
    ] {
        assert!(db.execute(sql).is_err(), "accepted: {sql}");
    }
    assert_eq!(
        db.execute("SELECT name,age FROM users").unwrap()[0].rows,
        vec![vec![Value::Text("Ada".into()), Value::Integer(18)]]
    );
}

#[test]
fn expressions_use_null_three_valued_logic_and_exact_arithmetic() {
    let mut db = Database::memory();
    let r = db.execute("SELECT 10+20, 10-20, 10*20, 10/20, 25%11, 0.1+0.2, NULL+1, FALSE AND NULL, TRUE OR NULL, NOT NULL, CASE WHEN NULL THEN 1 ELSE 2 END, COALESCE(NULL, 99), 5 BETWEEN 3 AND 8, 2 IN (1,2,NULL), 3 IN (1,2,NULL), 'ab'||'cd'").unwrap();
    assert_eq!(
        r[0].rows[0],
        vec![
            Value::Integer(30),
            Value::Integer(-10),
            Value::Integer(200),
            Value::Integer(0),
            Value::Integer(3),
            Value::Numeric("0.3".into()),
            Value::Null,
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Null,
            Value::Integer(2),
            Value::Integer(99),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Null,
            Value::Text("abcd".into())
        ]
    );
}

#[test]
fn rich_types_preserve_exact_values_and_validate_ranges() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE rich (s SMALLINT, b BIGINT, f DOUBLE PRECISION, n NUMERIC(20,4), ok BOOLEAN, word VARCHAR(3), bytes BYTEA, u UUID, j JSON, d DATE, t TIME, ts TIMESTAMP, tz TIMESTAMPTZ, tags TEXT[])").unwrap();
    let results = db.execute("INSERT INTO rich VALUES (32767, 9223372036854775807, 1.25, 1234567890123456.1234, TRUE, '猫猫猫', X'00ff', CAST('550e8400-e29b-41d4-a716-446655440000' AS UUID), CAST('{\"x\":1}' AS JSON), DATE '2024-02-29', TIME '12:30:00', TIMESTAMP '2024-02-29 12:30:00', CAST('2024-02-29T12:30:00+03:00' AS TIMESTAMPTZ), ARRAY['a','b']); SELECT n, ok, d, tags FROM rich").unwrap();
    assert_eq!(
        results.last().unwrap().rows,
        vec![vec![
            Value::Numeric("1234567890123456.1234".into()),
            Value::Boolean(true),
            Value::Date("2024-02-29".into()),
            Value::Array(vec![Value::Text("a".into()), Value::Text("b".into())])
        ]]
    );
    for sql in [
        "INSERT INTO rich (s) VALUES (32768)",
        "INSERT INTO rich (word) VALUES ('four')",
        "INSERT INTO rich (ok) VALUES (1)",
        "INSERT INTO rich (n) VALUES ('1.25')",
        "INSERT INTO rich (n) VALUES (0.00001)",
        "INSERT INTO rich (d) VALUES (DATE '2023-02-29')",
        "INSERT INTO rich (u) VALUES (CAST('not-a-uuid' AS UUID))",
        "INSERT INTO rich (tags) VALUES (ARRAY[1])",
    ] {
        assert!(db.execute(sql).is_err(), "accepted: {sql}");
    }
}

#[test]
fn all_tables_reject_wrong_types_and_failed_writes_leave_no_rows() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE people (id INTEGER, name TEXT)")
        .unwrap();
    assert!(
        db.execute("INSERT INTO people VALUES ('1', 'Ada')")
            .is_err()
    );
    assert!(db.execute("INSERT INTO people VALUES (1, 99)").is_err());
    assert!(
        db.execute("INSERT INTO people VALUES (2147483648, 'Ada')")
            .is_err()
    );
    assert!(db.execute("CREATE TABLE untyped (id)").is_err());
    assert!(
        db.execute("CREATE TABLE redundant (id INTEGER) STRICT")
            .is_err()
    );
    assert!(db.execute("CREATE TABLE unknown (id SOMETHING)").is_err());
    assert!(
        db.execute("SELECT * FROM people").unwrap()[0]
            .rows
            .is_empty()
    );
}
