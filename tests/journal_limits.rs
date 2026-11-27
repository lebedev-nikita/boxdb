//! Appends that the kernel cuts short, made by CLI processes with a file size
//! limit. These tests fork, and a forked child holds the locks of every open
//! readable file until it executes the CLI, so they run one at a time in a
//! test binary of their own.
#![cfg(unix)]
use boxdb::{CheckMode, Database, Format, OpenOptions, Value};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Lines appended since `before`.
fn appended(path: &std::path::Path, before: &str) -> String {
    let text = read(path);
    assert!(text.starts_with(before), "{text}");
    text[before.len()..].to_owned()
}

fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    db.execute(sql).unwrap().pop().unwrap().rows
}

/// Run the CLI with a file size limit, so the kernel cuts an append short.
/// With SIGXFSZ ignored the next write fails with EFBIG; otherwise the
/// process is killed in the middle of the append.
fn run_with_file_limit(
    path: &std::path::Path,
    limit: usize,
    ignore_signal: bool,
    sql: &str,
) -> std::process::Output {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_boxdb"));
    command.arg(path).args(["-e", sql]);
    // SAFETY: the closure runs in the forked child before exec and calls only
    // the async-signal-safe setrlimit and signal functions.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: limit as libc::rlim_t,
                rlim_max: limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if ignore_signal {
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            }
            Ok(())
        });
    }
    command.output().unwrap()
}

fn limited_append_setup() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,body TEXT); INSERT INTO t VALUES(1,'a')")
        .unwrap();
    db.execute("INSERT INTO t VALUES(2,'b')").unwrap();
    drop(db);
    let before = read(&path);
    let sql = format!("INSERT INTO t VALUES(3,'{}')", "x".repeat(8000));
    (temp, path, before, sql)
}

fn ids(path: &std::path::Path) -> Vec<Vec<Value>> {
    let mut db = Database::open_with_options(
        path,
        Format::Readable,
        OpenOptions {
            read_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    rows(&mut db, "SELECT id FROM t ORDER BY id")
}

#[test]
fn failed_appends_are_truncated_to_the_last_line() {
    let _serial = serial();
    let (_temp, path, before, sql) = limited_append_setup();
    let output = run_with_file_limit(&path, before.len() + 1000, true, &sql);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("append readable commit"), "{stderr}");
    assert_eq!(read(&path), before);
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(report.issues.is_empty(), "{report:?}");
    assert_eq!(
        ids(&path),
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn appends_cut_short_by_a_killed_process_are_removed_by_the_next_writer() {
    let _serial = serial();
    use std::os::unix::process::ExitStatusExt;
    let (_temp, path, before, sql) = limited_append_setup();
    let limit = before.len() + 1000;
    let output = run_with_file_limit(&path, limit, false, &sql);
    assert_eq!(output.status.signal(), Some(libc::SIGXFSZ), "{output:?}");
    // Linux stops at the limit; macOS counts it from the append offset.
    let torn = read(&path);
    assert!(torn.starts_with(&before));
    assert!(torn.len() > before.len() && torn.len() < before.len() + sql.len());
    assert!(!torn.ends_with('\n'));
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "BOXDB_INCOMPLETE_COMMIT"),
        "{report:?}"
    );
    assert_eq!(
        ids(&path),
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    let mut writer = Database::open(&path, Format::Readable).unwrap();
    writer.execute("INSERT INTO t VALUES(4,'d')").unwrap();
    drop(writer);
    assert_eq!(
        appended(&path, &before),
        "- {insert: t, rows: [{id: 4, body: d}]}\n"
    );
    let report = Database::check_file(&path, CheckMode::Full).unwrap();
    assert!(report.issues.is_empty(), "{report:?}");
}
