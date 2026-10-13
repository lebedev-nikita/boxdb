use std::io::Write;
use std::process::{Command, Stdio};
const BIN: &str = env!("CARGO_BIN_EXE_boxdb");

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
            "CREATE TABLE t(n INT4,name TEXT); INSERT INTO t VALUES(1,'a;b'); SELECT name FROM t",
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
        .write_all(b"CREATE TABLE t(n INTEGER);\nINSERT INTO t VALUES(7);\nSELECT n FROM t;")
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
    child.stdin.take().unwrap().write_all(b"CREATE TABLE t(n INTEGER);\nCREATE TABLE log(n INTEGER);\nCREATE TRIGGER track AFTER INSERT ON t BEGIN\n INSERT INTO log VALUES(NEW.n);\n INSERT INTO log VALUES(NEW.n+1);\nEND;\nINSERT INTO t VALUES(5);\nSELECT n FROM log ORDER BY n;\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "n\n5\n6");
}
