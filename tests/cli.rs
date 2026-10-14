use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
const BIN: &str = env!("CARGO_BIN_EXE_boxdb");

#[test]
fn cli_provides_bundled_extensions_without_loading_shared_libraries() {
    let output = Command::new(BIN)
        .args(["--json", "-e", "SELECT decimal_div('0.3','1',1,'half_even') AS amount, text_lower('ПРИВЕТ') AS word, regexp_substr('order #123','\\d+') AS digits, vec_distance_cosine('[1,0]','[0,1]') AS distance"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["rows"][0],
        serde_json::json!([
            {"Decimal": "0.3"}, {"Text": "привет"}, {"Text": "123"}, {"Real": 1.0},
        ])
    );
}

#[test]
fn cli_reports_boxdb_name_and_release_version() {
    let output = Command::new(BIN).arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        concat!("boxdb ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn cli_prints_standalone_values_with_column_names() {
    let output = Command::new(BIN)
        .args(["-e", "VALUES (1,'Ada'),(2,'Grace')"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "column1\tcolumn2\n1\tAda\n2\tGrace"
    );
}

#[test]
fn cli_executes_sql_reopens_readable_files_and_reports_type_errors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let first = Command::new(BIN)
        .arg(&path)
        .args([
            "--format",
            "readable",
            "-e",
            "CREATE TABLE t(n INT32,name TEXT); INSERT INTO t VALUES(1,'a;b'); SELECT name FROM t",
        ])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(String::from_utf8(first.stdout).unwrap().trim(), "name\na;b");
    let second = Command::new(BIN)
        .arg(&path)
        .args(["--json", "-e", "SELECT n FROM t"])
        .output()
        .unwrap();
    assert!(second.status.success());
    let result: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(result["rows"][0][0]["Integer"], 1);
    let bad = Command::new(BIN)
        .arg(&path)
        .args(["-e", "INSERT INTO t VALUES(2147483648,'wrong')"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("CHECK"));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("---\nkind: database\n")
    );
}

#[test]
fn cli_reads_sql_from_stdin_and_converts_without_overwriting_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.boxdb");
    let dest = temp.path().join("dest.yaml");
    let mut child = Command::new(BIN)
        .arg(&source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE TABLE t(n INT64);\nINSERT INTO t VALUES(7);\nSELECT n FROM t;")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "n\n7");
    let convert = Command::new(BIN)
        .arg("convert")
        .arg(&source)
        .arg(&dest)
        .args(["--format", "readable"])
        .output()
        .unwrap();
    assert!(
        convert.status.success(),
        "{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    let again = Command::new(BIN)
        .arg("convert")
        .arg(&source)
        .arg(&dest)
        .args(["--format", "binary"])
        .output()
        .unwrap();
    assert!(!again.status.success());
}

#[test]
fn cli_accepts_multiline_sqlite_trigger_bodies() {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"CREATE TABLE t(n INT64);\nCREATE TABLE log(n INT64);\nCREATE TRIGGER track AFTER INSERT ON t BEGIN\n INSERT INTO log VALUES(NEW.n);\n INSERT INTO log VALUES(NEW.n+1);\nEND;\nINSERT INTO t VALUES(5);\nSELECT n FROM log ORDER BY n;\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "n\n5\n6");
}

struct Session {
    child: Child,
    results: Receiver<serde_json::Value>,
}
impl Session {
    fn open(path: &std::path::Path) -> Self {
        let mut child = Command::new(BIN)
            .arg(path)
            .args(["--format", "readable", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, results) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let result = serde_json::from_str(&line).unwrap();
                if sender.send(result).is_err() {
                    break;
                }
            }
        });
        Self { child, results }
    }
    fn send(&mut self, sql: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{sql}").unwrap();
    }
    fn execute(&mut self, sql: &str) -> serde_json::Value {
        self.send(sql);
        self.results
            .recv_timeout(Duration::from_secs(20))
            .expect("CLI query acknowledgement")
    }
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn cli_crash_preserves_acknowledged_commits_and_discards_pending_writes() {
    use boxdb::{Database, Format, Value};
    for commit in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("crash.yaml");
        let mut session = Session::open(&path);
        assert_eq!(
            session.execute(
                "CREATE TABLE t(n INT64); INSERT INTO t VALUES(1); SELECT COUNT(*) FROM t;"
            )["rows"][0][0]["Integer"],
            1
        );
        let sql = if commit {
            "BEGIN; INSERT INTO t VALUES(2); COMMIT; SELECT COUNT(*) FROM t;"
        } else {
            "BEGIN; INSERT INTO t VALUES(2); SELECT COUNT(*) FROM t;"
        };
        assert_eq!(session.execute(sql)["rows"][0][0]["Integer"], 2);
        session.kill();
        let mut db = Database::open(&path, Format::Readable).unwrap();
        let expected = if commit {
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        } else {
            vec![vec![Value::Integer(1)]]
        };
        assert_eq!(
            db.execute("SELECT n FROM t ORDER BY n").unwrap()[0].rows,
            expected
        );
    }
}

#[test]
fn interrupting_readable_commit_leaves_a_complete_old_or_new_database() {
    use boxdb::{Database, Format, Value};
    // Different interruption delays exercise publication without depending on
    // private callbacks or temporary-file names. Either commit outcome is valid.
    for delay in [0, 2, 10, 25] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("interrupted.yaml");
        let mut session = Session::open(&path);
        assert_eq!(session.execute("CREATE TABLE t(id INT64 PRIMARY KEY,body TEXT,revision INT64); BEGIN; WITH RECURSIVE rows(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<1500) INSERT INTO t SELECT n,printf('%04096d',0),0 FROM rows; COMMIT; SELECT COUNT(*) FROM t;")["rows"][0][0]["Integer"], 1500);
        assert_eq!(
            session.execute(
                "BEGIN; UPDATE t SET revision=1 WHERE id=1; SELECT revision FROM t WHERE id=1;"
            )["rows"][0][0]["Integer"],
            1
        );
        session.send("COMMIT; SELECT COUNT(*) FROM t;");
        std::thread::sleep(Duration::from_millis(delay));
        session.kill();
        let mut db = Database::open(&path, Format::Readable).unwrap();
        let row = &db
            .execute("SELECT COUNT(*),SUM(id),SUM(length(body)),SUM(revision) FROM t")
            .unwrap()[0]
            .rows[0];
        let old = vec![
            Value::Integer(1500),
            Value::Integer(1_125_750),
            Value::Integer(6_144_000),
            Value::Integer(0),
        ];
        let new = vec![
            Value::Integer(1500),
            Value::Integer(1_125_750),
            Value::Integer(6_144_000),
            Value::Integer(1),
        ];
        assert!(row == &old || row == &new, "delay={delay}: {row:?}");
        assert_eq!(
            db.execute("PRAGMA quick_check").unwrap()[0].rows,
            vec![vec![Value::Text("ok".into())]]
        );
    }
}
