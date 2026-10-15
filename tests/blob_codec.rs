#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, Value};

#[test]
fn native_values_and_bytea_have_exactly_one_storage_tag_byte() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    for (sql, expected) in [
        ("CAST('12' AS DECIMAL)", b"d12".to_vec()),
        (
            "CAST('00000000-0000-0000-0000-000000000000' AS UUID)",
            [b"u".as_slice(), &[0; 16]].concat(),
        ),
        (
            "CAST('1970-01-01T00:00:00Z' AS INSTANT)",
            [b"i".as_slice(), &[0; 12]].concat(),
        ),
        (
            "CAST('1970-01-01T00:00:00' AS DATETIME)",
            [b"t".as_slice(), &[0; 12]].concat(),
        ),
        (
            "CAST('0 seconds' AS INTERVAL)",
            [b"v".as_slice(), &[0; 16]].concat(),
        ),
        ("X'643132'", b"bd12".to_vec()),
        ("X''", b"b".to_vec()),
    ] {
        let actual: Vec<u8> = connection
            .query_row(&format!("SELECT {sql}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(actual, expected, "{sql}");
    }
}

#[test]
fn bytea_payloads_are_never_confused_with_native_tags_and_survive_conversion() {
    let payloads = [
        vec![],
        b"d12".to_vec(),
        b"b".to_vec(),
        vec![b'u'; 17],
        b"\0BDd12".to_vec(),
        vec![0, 255],
    ];
    let mut db = Database::memory();
    db.execute("CREATE TABLE blobs(id INTEGER PRIMARY KEY,v BYTEA); CREATE UNIQUE INDEX blobs_v ON blobs(v)").unwrap();
    for payload in &payloads {
        db.execute_params(
            "INSERT INTO blobs(v) VALUES(?)",
            &[Value::Bytea(payload.clone())],
        )
        .unwrap();
        assert_eq!(
            db.execute_params(
                "SELECT typeof(?1),?1,length(?1),hex(?1)",
                &[Value::Bytea(payload.clone())]
            )
            .unwrap()[0]
                .rows,
            vec![vec![
                Value::Text("blob".into()),
                Value::Bytea(payload.clone()),
                Value::Integer(payload.len() as i64),
                Value::Text(hex::encode_upper(payload))
            ]]
        );
    }
    let expected = payloads
        .iter()
        .map(|p| vec![Value::Bytea(p.clone())])
        .collect::<Vec<_>>();
    for format in [Format::Binary, Format::Readable] {
        let snapshot = files::converted_bytes(&mut db, format).unwrap();
        if format == Format::Readable {
            let text = std::str::from_utf8(&snapshot).unwrap();
            assert!(text.contains("blob: '00ff'"), "{text}");
            assert!(!text.contains("blob: '6200ff'"), "{text}");
        }
        let mut restored = files::open_bytes(&snapshot).unwrap();
        assert_eq!(
            restored.execute("SELECT v FROM blobs ORDER BY id").unwrap()[0].rows,
            expected
        );
        assert_eq!(
            restored.execute("PRAGMA integrity_check").unwrap()[0].rows,
            vec![vec![Value::Text("ok".into())]]
        );
    }
}

#[test]
fn incremental_bytea_io_uses_payload_offsets_and_preserves_the_tag() {
    use rusqlite::ffi;
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE blobs(v BLOB); INSERT INTO blobs VALUES(zeroblob(4)),(X'643132')",
        )
        .unwrap();
    struct Blob(*mut ffi::sqlite3_blob);
    impl Drop for Blob {
        fn drop(&mut self) {
            unsafe {
                ffi::sqlite3_blob_close(self.0);
            }
        }
    }
    let mut raw = std::ptr::null_mut();
    // SAFETY: connection, blob handle and buffers remain live for each call.
    unsafe {
        assert_eq!(
            ffi::sqlite3_blob_open(
                connection.handle(),
                c"main".as_ptr(),
                c"blobs".as_ptr(),
                c"v".as_ptr(),
                1,
                1,
                &mut raw
            ),
            ffi::SQLITE_OK
        );
        let blob = Blob(raw);
        assert_eq!(ffi::sqlite3_blob_bytes(blob.0), 4);
        assert_eq!(
            ffi::sqlite3_blob_write(blob.0, b"abcd".as_ptr().cast(), 4, 0),
            ffi::SQLITE_OK
        );
        let mut bytes = [0; 4];
        assert_eq!(
            ffi::sqlite3_blob_read(blob.0, bytes.as_mut_ptr().cast(), 4, 0),
            ffi::SQLITE_OK
        );
        assert_eq!(&bytes, b"abcd");
        assert_eq!(
            ffi::sqlite3_blob_write(blob.0, b"!".as_ptr().cast(), 1, 4),
            ffi::SQLITE_ERROR
        );
        assert_eq!(ffi::sqlite3_blob_reopen(blob.0, 2), ffi::SQLITE_OK);
        assert_eq!(ffi::sqlite3_blob_bytes(blob.0), 3);
    }
    let values = connection
        .prepare("SELECT v FROM blobs ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(values, vec![b"babcd".to_vec(), b"bd12".to_vec()]);
}

#[test]
fn codec_versions_are_database_metadata_and_unknown_versions_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codec.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE blobs(v BYTEA)").unwrap();
    let mut restored =
        files::open_bytes(&files::converted_bytes(&mut db, Format::Binary).unwrap()).unwrap();
    assert_eq!(
        restored
            .execute("SELECT version FROM __boxdb_codec")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    let readable = files::converted_bytes(&mut db, Format::Readable).unwrap();
    let text = String::from_utf8(readable).unwrap();
    assert!(text.contains("codec_version: 1"), "{text}");
    assert!(
        files::open_bytes(
            text.replace("codec_version: 1", "codec_version: 99")
                .as_bytes()
        )
        .is_err()
    );
    drop(db);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE __boxdb_codec SET version=99", [])
        .unwrap();
    assert!(Database::open(&path, Format::Binary).is_err());
}

#[test]
fn stored_blob_lengths_and_numeric_conversions_use_payloads() {
    let mut db = Database::memory();
    db.execute("PRAGMA page_size=512; CREATE TABLE blobs(v BYTEA); INSERT INTO blobs VALUES(zeroblob(8192)),(X'313233')").unwrap();
    assert_eq!(
        db.execute("SELECT length(v),octet_length(v),typeof(v) FROM blobs ORDER BY rowid")
            .unwrap()[0]
            .rows,
        vec![
            vec![
                Value::Integer(8192),
                Value::Integer(8192),
                Value::Text("blob".into())
            ],
            vec![
                Value::Integer(3),
                Value::Integer(3),
                Value::Text("blob".into())
            ]
        ]
    );
    assert_eq!(db.execute("SELECT v+0,v%2,CASE WHEN v THEN 1 ELSE 0 END,CAST(v AS INTEGER) FROM blobs WHERE rowid=2").unwrap()[0].rows,
        vec![vec![Value::Integer(123),Value::Integer(1),Value::Integer(1),Value::Integer(123)]]);
}

#[test]
fn zero_blob_text_casts_keep_sqlite_encoding_semantics() {
    for (encoding, expected) in [
        ("UTF-8", "\0\0\0\0"),
        ("UTF-16le", "\0\0"),
        ("UTF-16be", "\0\0"),
    ] {
        let mut db = Database::memory();
        db.execute(&format!("PRAGMA encoding='{encoding}'"))
            .unwrap();
        assert_eq!(db.execute("SELECT CAST(zeroblob(4) AS TEXT),CAST(X'00000000' AS TEXT),CAST(zeroblob(0) AS TEXT)").unwrap()[0].rows,
            vec![vec![Value::Text("\0\0\0\0".into()),Value::Text(expected.into()),Value::Text(String::new())]], "{encoding}");
    }
}
