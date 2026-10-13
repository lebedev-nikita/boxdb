use boxdb::{Database, Format, Value};

#[test]
fn literal_inserts_match_the_full_parser_and_fail_atomically() {
    for sql in [
        "INSERT INTO t VALUES(NULL,+12,.25,TRUE,'雪''x'),(7,-12,1e2,FALSE,'')",
        "insert into T values(1,9223372036854775807,-.5,null,'x');",
        "INSERT INTO t VALUES(1,-9223372036854775808,1.,true,'x')",
        "INSERT INTO t VALUES(1,1,1E-2,true,'x')",
        "INSERT INTO t VALUES(1,1,1e+2,true,'x')",
        "INSERT INTO t VALUES(1,1,0.1+0.2,true,'x')",
        "INSERT INTO t(id,n) VALUES(1,2)",
        "INSERT INTO \"t\" VALUES(1,1,1,true,'a\\b')",
        "INSERT INTO t VALUES(1,1,1,true,'x') -- trailing comment",
        "INSERT INTO t VALUES(1,1,1,true,'x'),(1,2,2,false,'y')",
        "INSERT INTO t VALUES(1,1,1,true,'x'),(2,'wrong',2,false,'y')",
        "INSERT INTO t VALUES(1,1,1,true,'x'),(2,2,2,false)",
        "INSERT INTO t VALUES(1,1,1e,true,'x')",
        "INSERT INTO t VALUES(1,1,1,true,'x'); garbage",
        "INSERT INTO t VALUES(1,1,1,true,'unterminated)",
    ] {
        let schema =
            "CREATE TABLE t(id INTEGER PRIMARY KEY,n BIGINT,v NUMERIC,ok BOOLEAN,body TEXT)";
        let mut fast = Database::memory();
        let mut reference = Database::memory();
        fast.execute(schema).unwrap();
        reference.execute(schema).unwrap();
        let actual = fast.execute(sql);
        let expected = reference.execute(&format!("-- force full parser\n{sql}"));
        assert_eq!(actual.is_ok(), expected.is_ok(), "{sql}");
        if let (Ok(a), Ok(b)) = (actual, expected) {
            assert_eq!(a, b, "{sql}");
        }
        assert_eq!(
            fast.export(Format::Binary).unwrap(),
            reference.export(Format::Binary).unwrap(),
            "{sql}"
        );
    }
}

fn assert_query_matches_generic(db: &mut Database, sql: &str) {
    let fast = db.execute(sql);
    // Multiple statements use the general executor rather than a bound plan.
    let generic = db.execute(&format!("{sql}; SELECT 1"));
    assert_eq!(fast.is_ok(), generic.is_ok(), "{sql}");
    if let (Ok(fast), Ok(generic)) = (fast, generic) {
        assert_eq!(fast[0], generic[0], "{sql}");
    }
}

#[test]
fn streaming_aggregates_match_generic_null_order_limit_and_overflow_semantics() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(g BIGINT,n BIGINT); INSERT INTO t VALUES(2,3),(NULL,NULL),(1,4),(2,NULL),(NULL,5),(1,-2)").unwrap();
    let queries = [
        "SELECT g,COUNT(*),COUNT(n),SUM(n) FROM t GROUP BY g",
        "SELECT g,COUNT(n) AS c,SUM(n) AS s FROM t GROUP BY g ORDER BY c DESC,g NULLS LAST",
        "SELECT g,SUM(n) FROM t GROUP BY g ORDER BY 1 DESC NULLS FIRST LIMIT 2 OFFSET 1",
        "SELECT COUNT(*),COUNT(n),SUM(n) FROM t",
        "SELECT g FROM t GROUP BY g ORDER BY g",
        "SELECT COUNT(DISTINCT n),SUM(n) FROM t",
        "SELECT g,SUM(n) FROM t WHERE n>0 GROUP BY g HAVING SUM(n)>1",
        "SELECT q.g,SUM(q.n) FROM t AS q GROUP BY q.g ORDER BY 1",
    ];
    for sql in queries {
        assert_query_matches_generic(&mut db, sql);
    }
    db.execute("DELETE FROM t").unwrap();
    for sql in queries {
        assert_query_matches_generic(&mut db, sql);
    }
    db.execute("INSERT INTO t VALUES(1,9223372036854775807),(1,1)")
        .unwrap();
    assert_query_matches_generic(&mut db, "SELECT SUM(n) FROM t");
    assert!(db.execute("SELECT SUM(n) FROM t").is_err());
}

#[test]
fn cached_plans_follow_schema_savepoint_and_parameter_changes() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'old')")
        .unwrap();
    let sql = "SELECT body FROM t WHERE id=1";
    assert_eq!(
        db.execute(sql).unwrap()[0].rows[0][0],
        Value::Text("old".into())
    );
    db.execute("BEGIN; SAVEPOINT s; DROP TABLE t; CREATE TABLE t(body TEXT,id INTEGER PRIMARY KEY); INSERT INTO t VALUES('new',1)").unwrap();
    assert_eq!(
        db.execute(sql).unwrap()[0].rows[0][0],
        Value::Text("new".into())
    );
    db.execute("ROLLBACK TO s").unwrap();
    assert_eq!(
        db.execute(sql).unwrap()[0].rows[0][0],
        Value::Text("old".into())
    );
    db.execute("CREATE INDEX t_body ON t(body)").unwrap();
    assert_query_matches_generic(&mut db, "SELECT id FROM t WHERE body='old'");
    db.execute("ROLLBACK").unwrap();
    assert_query_matches_generic(&mut db, "SELECT id FROM t WHERE body='old'");
    for id in [1, 2, 1] {
        let result = db
            .execute_params("SELECT body FROM t WHERE id=?", &[Value::Integer(id)])
            .unwrap();
        assert_eq!(result[0].rows.len(), usize::from(id == 1));
    }
}

#[test]
fn paged_rows_preserve_unique_indexes_through_compaction_and_rollback() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER UNIQUE); CREATE INDEX t_id ON t(id)",
    )
    .unwrap();
    let values = (1..=5000)
        .map(|n| format!("({n},{n})"))
        .collect::<Vec<_>>()
        .join(",");
    db.execute(&format!("INSERT INTO t VALUES{values}"))
        .unwrap();
    let initial = db.export(Format::Binary).unwrap();
    db.execute("BEGIN; DELETE FROM t WHERE id<=3000; UPDATE t SET n=n+10000 WHERE id>3000")
        .unwrap();
    assert!(db.execute("UPDATE t SET n=15000 WHERE id=4000").is_err());
    assert_query_matches_generic(
        &mut db,
        "SELECT id,n FROM t WHERE id>=4998 ORDER BY id DESC",
    );
    db.execute("ROLLBACK").unwrap();
    assert_eq!(db.export(Format::Binary).unwrap(), initial);
    db.execute("DELETE FROM t WHERE id<=3000; UPDATE t SET n=9001-id WHERE id>=4000 AND id<=5000")
        .unwrap();
    assert!(db.execute("INSERT INTO t VALUES(6000,4001)").is_err());
    db.execute("INSERT INTO t VALUES(6000,6000)").unwrap();
    assert_query_matches_generic(
        &mut db,
        "SELECT id,n FROM t WHERE id>=4998 ORDER BY id DESC",
    );
    let mut restored = Database::from_snapshot(&db.export(Format::Binary).unwrap()).unwrap();
    assert_eq!(
        restored.execute("SELECT * FROM t ORDER BY id").unwrap(),
        db.execute("SELECT * FROM t ORDER BY id").unwrap()
    );
}
