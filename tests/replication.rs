use boxdb::replication::{Credentials, Replicator, S3Config};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    etag: String,
}
struct Server {
    endpoint: String,
    objects: Arc<Mutex<BTreeMap<String, Object>>>,
    fault: Arc<Mutex<Option<(String, u16)>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Mutex::new(BTreeMap::<String, Object>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let fault = Arc::new(Mutex::new(None));
        let fail = fault.clone();
        let data = objects.clone();
        let done = stop.clone();
        let join = thread::spawn(move || {
            for stream in listener.incoming() {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(mut stream) = stream {
                    serve(&mut stream, &data, &fail);
                }
            }
        });
        Self {
            endpoint,
            objects,
            fault,
            stop,
            join: Some(join),
        }
    }
    fn replicator(&self) -> Replicator {
        Replicator::new(S3Config {
            endpoint: self.endpoint.clone(),
            region: "us-east-1".into(),
            bucket: "test".into(),
            prefix: "database".into(),
            credentials: Credentials {
                access_key_id: "test-key".into(),
                secret_access_key: "test-secret".into(),
                session_token: Some("test-session".into()),
            },
        })
        .unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}
fn serve(
    stream: &mut TcpStream,
    objects: &Arc<Mutex<BTreeMap<String, Object>>>,
    fault: &Arc<Mutex<Option<(String, u16)>>>,
) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = vec![];
    let mut buffer = [0u8; 4096];
    let end = loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
    let mut lines = head.lines();
    let request = lines.next().unwrap().split_whitespace().collect::<Vec<_>>();
    let headers = lines
        .filter_map(|s| s.split_once(':'))
        .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    let length = headers
        .get("content-length")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() - end < length {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    assert!(
        headers
            .get("authorization")
            .unwrap()
            .starts_with("AWS4-HMAC-SHA256 Credential=test-key/")
    );
    assert_eq!(headers.get("x-amz-security-token").unwrap(), "test-session");
    verify_signature(&request, &headers, &bytes[end..end + length]);
    if request[0] == "PUT" {
        let mut fault = fault.lock().unwrap();
        if fault
            .as_ref()
            .is_some_and(|(prefix, _)| request[1].contains(prefix))
        {
            let (_, code) = fault.take().unwrap();
            stream.write_all(format!("HTTP/1.1 {code} Injected failure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
            return;
        }
    }
    let mut objects = objects.lock().unwrap();
    let mut status = "200 OK";
    let mut body = vec![];
    let mut etag = String::new();
    match request[0] {
        "GET" => {
            if let Some(object) = objects.get(request[1]) {
                body = object.bytes.clone();
                etag = object.etag.clone();
            } else {
                status = "404 Not Found";
            }
        }
        "PUT" => {
            if headers.get("if-none-match").is_some_and(|s| s == "*")
                && objects.contains_key(request[1])
                || headers.get("if-match").is_some_and(|expected| {
                    objects.get(request[1]).is_none_or(|o| o.etag != *expected)
                })
            {
                status = "412 Precondition Failed";
            } else {
                body.clear();
                let value = bytes[end..end + length].to_vec();
                etag = format!("\"{}\"", hex::encode(sha2::Sha256::digest(&value)));
                objects.insert(
                    request[1].into(),
                    Object {
                        bytes: value,
                        etag: etag.clone(),
                    },
                );
            }
        }
        _ => status = "405 Method Not Allowed",
    }
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
}
use sha2::Digest;
fn verify_signature(request: &[&str], headers: &BTreeMap<String, String>, body: &[u8]) {
    use hmac::{Hmac, Mac};
    fn sign(key: &[u8], body: &[u8]) -> Vec<u8> {
        let mut h = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
        h.update(body);
        h.finalize().into_bytes().to_vec()
    }
    let hash = |bytes: &[u8]| hex::encode(sha2::Sha256::digest(bytes));
    assert_eq!(headers["x-amz-content-sha256"], hash(body));
    let timestamp = &headers["x-amz-date"];
    let scope = format!("{}/us-east-1/s3/aws4_request", &timestamp[..8]);
    let auth = &headers["authorization"];
    let fields = auth
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .unwrap()
        .split(", ")
        .filter_map(|s| s.split_once('='))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields["Credential"], format!("test-key/{scope}"));
    let signed = fields["SignedHeaders"].split(';').collect::<Vec<_>>();
    assert!(
        signed.contains(&"host")
            && signed.contains(&"x-amz-date")
            && signed.contains(&"x-amz-content-sha256")
            && signed.contains(&"x-amz-security-token")
    );
    if headers.contains_key("if-match") {
        assert!(signed.contains(&"if-match"));
    }
    if headers.contains_key("if-none-match") {
        assert!(signed.contains(&"if-none-match"));
    }
    let canonical_headers = signed
        .iter()
        .map(|key| format!("{key}:{}", headers[*key]))
        .collect::<Vec<_>>()
        .join("\n");
    let canonical = [
        request[0],
        request[1],
        "",
        &canonical_headers,
        "",
        fields["SignedHeaders"],
        &hash(body),
    ]
    .join("\n");
    let key = sign(
        &sign(
            &sign(
                &sign(b"AWS4test-secret", &timestamp.as_bytes()[..8]),
                b"us-east-1",
            ),
            b"s3",
        ),
        b"aws4_request",
    );
    let message = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        hash(canonical.as_bytes())
    );
    assert_eq!(
        fields["Signature"],
        hex::encode(sign(&key, message.as_bytes()))
    );
}

#[test]
fn s3_roundtrip_restarts_and_uploads_only_changed_chunks() {
    let server = Server::new();
    let replicator = server.replicator();
    let mut data = vec![b'a'; 150_000];
    let first = replicator.push(&data).unwrap();
    assert_eq!(replicator.restore(None).unwrap(), data);
    assert_eq!(replicator.push(&data).unwrap().uploaded_chunks, 0);
    data[80_000] = b'b';
    let second = server.replicator().push(&data).unwrap();
    assert_eq!(second.uploaded_chunks, 1);
    assert_eq!(server.replicator().restore(None).unwrap(), data);
    assert_eq!(
        server.replicator().restore(Some(&first.id)).unwrap(),
        vec![b'a'; 150_000]
    );
}

#[test]
fn cli_replication_and_restore_reopen_both_formats() {
    use boxdb::{Database, Format, Value};
    use std::process::Command;
    let server = Server::new();
    let temp = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_boxdb");
    for (i, format) in [Format::Binary, Format::Readable].into_iter().enumerate() {
        let source = temp.path().join(format!("source{i}.boxdb"));
        let destination = temp.path().join(format!("restored{i}.boxdb"));
        {
            let mut db = Database::open(&source, format).unwrap();
            db.execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(7)")
                .unwrap();
        }
        let flags = [
            "--bucket",
            "test",
            "--prefix",
            "database",
            "--endpoint",
            server.endpoint.as_str(),
            "--region",
            "us-east-1",
        ];
        let configure = |command: &mut Command| {
            command
                .env("AWS_ACCESS_KEY_ID", "test-key")
                .env("AWS_SECRET_ACCESS_KEY", "test-secret")
                .env("AWS_SESSION_TOKEN", "test-session");
        };
        let mut push = Command::new(bin);
        push.arg("replicate").arg(&source).arg("--once").args(flags);
        configure(&mut push);
        let output = push.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut restore = Command::new(bin);
        restore.arg("restore").arg(&destination).args(flags);
        configure(&mut restore);
        let output = restore.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut db = Database::open(&destination, Format::Binary).unwrap();
        assert_eq!(db.format(), Some(format));
        assert_eq!(
            db.execute("SELECT n FROM t").unwrap()[0].rows,
            vec![vec![Value::Integer(7)]]
        );
    }
}

#[test]
fn failed_uploads_preserve_latest_revision_and_can_be_retried() {
    let server = Server::new();
    let replicator = server.replicator();
    replicator.push(b"old").unwrap();
    *server.fault.lock().unwrap() = Some(("revisions/".into(), 500));
    assert!(replicator.push(b"new").is_err());
    assert_eq!(replicator.restore(None).unwrap(), b"old");
    assert_eq!(replicator.push(b"new").unwrap().uploaded_chunks, 0);
    assert_eq!(replicator.restore(None).unwrap(), b"new");
    *server.fault.lock().unwrap() = Some(("latest.json".into(), 412));
    assert!(replicator.push(b"third").is_err());
    assert_eq!(replicator.restore(None).unwrap(), b"new");
    replicator.push(b"third").unwrap();
    assert_eq!(replicator.restore(None).unwrap(), b"third");
}

#[test]
fn restore_rejects_corrupted_chunks_and_invalid_revision_paths() {
    let server = Server::new();
    let replicator = server.replicator();
    replicator.push(b"valid data").unwrap();
    {
        let mut objects = server.objects.lock().unwrap();
        let object = objects
            .iter_mut()
            .find(|(key, _)| key.contains("/chunks/"))
            .unwrap()
            .1;
        object.bytes[0] ^= 1;
    }
    assert!(
        replicator
            .restore(None)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    assert!(replicator.restore(Some("../../latest.json")).is_err());
}

#[test]
fn signed_s3_requests_preserve_unicode_and_spaces_in_object_prefixes() {
    let server = Server::new();
    let replicator = Replicator::new(S3Config {
        endpoint: server.endpoint.clone(),
        region: "us-east-1".into(),
        bucket: "test".into(),
        prefix: "db 猫/percent%".into(),
        credentials: Credentials {
            access_key_id: "test-key".into(),
            secret_access_key: "test-secret".into(),
            session_token: Some("test-session".into()),
        },
    })
    .unwrap();
    replicator.push(b"data").unwrap();
    assert_eq!(replicator.restore(None).unwrap(), b"data");
    assert!(
        server
            .objects
            .lock()
            .unwrap()
            .keys()
            .all(|key| key.starts_with("/test/db%20%E7%8C%AB/percent%25/"))
    );
}

#[test]
fn credentials_load_static_profiles_and_redact_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("credentials");
    std::fs::write(&path,"[other]\naws_access_key_id = wrong\n[backup]\naws_access_key_id = test-key\naws_secret_access_key = test-secret\naws_session_token = test-session\n").unwrap();
    let credentials = Credentials::from_profile(&path, "backup").unwrap();
    assert_eq!(credentials.access_key_id, "test-key");
    assert!(!format!("{credentials:?}").contains("test-secret"));
    assert!(!format!("{credentials:?}").contains("test-session"));
    assert!(Credentials::from_profile(&path, "missing").is_err());
}

#[test]
fn continuous_cli_streams_commits_while_the_writer_remains_open() {
    use boxdb::{Database, Format, Value};
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let server = Server::new();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("live.jsonl");
    let mut writer = Database::open(&path, Format::Readable).unwrap();
    writer
        .execute("CREATE TABLE t(n INTEGER); INSERT INTO t VALUES(1)")
        .unwrap();
    let mut process = Running(
        Command::new(env!("CARGO_BIN_EXE_boxdb"))
            .arg("replicate")
            .arg(&path)
            .args([
                "--bucket",
                "test",
                "--prefix",
                "database",
                "--endpoint",
                &server.endpoint,
                "--region",
                "us-east-1",
                "--interval-ms",
                "20",
            ])
            .env("AWS_ACCESS_KEY_ID", "test-key")
            .env("AWS_SECRET_ACCESS_KEY", "test-secret")
            .env("AWS_SESSION_TOKEN", "test-session")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = process.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first: serde_json::Value =
        serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    writer.execute("BEGIN; UPDATE t SET n=2").unwrap();
    let mut remote = Database::from_snapshot(&server.replicator().restore(None).unwrap()).unwrap();
    assert_eq!(
        remote.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
    writer.execute("COMMIT").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        let revision: serde_json::Value = serde_json::from_str(&line).unwrap();
        if revision["id"] != first["id"] {
            break;
        }
    }
    let mut remote = Database::from_snapshot(&server.replicator().restore(None).unwrap()).unwrap();
    assert_eq!(
        remote.execute("SELECT n FROM t").unwrap()[0].rows,
        vec![vec![Value::Integer(2)]]
    );
    drop(process);
    reader.join().unwrap();
}

#[test]
fn cli_loads_selected_aws_profile_when_environment_keys_are_absent() {
    use boxdb::{Database, Format};
    let server = Server::new();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.boxdb");
    drop(Database::open(&path, Format::Binary).unwrap());
    let credentials = temp.path().join("credentials");
    std::fs::write(&credentials,"[backup]\naws_access_key_id=test-key\naws_secret_access_key=test-secret\naws_session_token=test-session\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_boxdb"))
        .arg("replicate")
        .arg(path)
        .args([
            "--once",
            "--bucket",
            "test",
            "--prefix",
            "database",
            "--endpoint",
            &server.endpoint,
            "--region",
            "us-east-1",
        ])
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env("AWS_PROFILE", "backup")
        .env("AWS_SHARED_CREDENTIALS_FILE", credentials)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
