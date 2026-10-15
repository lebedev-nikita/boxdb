#[allow(dead_code)]
#[path = "common/files.rs"]
mod files;
use boxdb::{Database, Format, SchemaFormat, Value};

#[test]
fn decimal_sql_api_exposes_only_native_rounding_and_division() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute(
            "SELECT name,narg FROM pragma_function_list WHERE name GLOB 'decimal*' ORDER BY name"
        )
        .unwrap()[0]
            .rows,
        vec![
            vec![Value::Text("decimal_div".into()), Value::Integer(4)],
            vec![Value::Text("decimal_round".into()), Value::Integer(3)],
        ]
    );
    assert_eq!(
        db.execute(
            "SELECT decimal_round('123.456',2,'half_even'),decimal_div('1','3',2,'half_even')"
        )
        .unwrap()[0]
            .rows,
        vec![vec![
            Value::Decimal("123.46".into()),
            Value::Decimal("0.33".into())
        ]]
    );
    assert!(
        db.execute("SELECT value FROM (SELECT '2' AS value) ORDER BY value COLLATE decimal")
            .is_err()
    );
}

#[test]
fn unicode_text_functions_and_collation_work_in_sql() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT text_lower('ПРИВЕТ'), text_upper('straße'), text_split('a/b/c','/',2), 'Ёж' = 'ёж' COLLATE text_nocase").unwrap()[0].rows,
        vec![vec![Value::Text("привет".into()), Value::Text("STRAẞE".into()), Value::Text("b".into()), Value::Integer(1)]]);
}

#[test]
fn text_like_matches_unicode_substrings_at_the_start() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT text_like('%донат%', 'ДОНАТ')").unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn text_like_wildcards_match_empty_spans_and_backtrack_over_unicode_characters() {
    let mut db = Database::memory();
    let cases = [
        ("%abc%", "ABC", 1),
        ("%донат%", "первый ДОНАТ!", 1),
        ("%ёж%", "ЁЖ", 1),
        ("%cóm_ está_%", "CÓMO ESTÁS", 1),
        ("a%b", "ab", 1),
        ("a%%b", "ab", 1),
        ("%%a", "a", 1),
        ("a%", "a", 1),
        ("%ab", "aab", 1),
        ("%aab", "aaab", 1),
        ("%ab%bc", "abbc", 1),
        ("%ab%bc", "abc", 0),
        ("%aa", "a", 0),
        ("%a%a", "a", 0),
        ("a%b", "ac", 0),
        ("%x", "abc", 0),
        ("%_%", "я", 1),
        ("%_%", "", 0),
        ("_", "😀", 1),
        ("_", "ёж", 0),
        ("%_ж", "ЁЖ", 1),
        ("%__", "я", 0),
        ("%", "", 1),
        ("%%", "", 1),
        ("%", "abc", 1),
        ("", "", 1),
        ("", "a", 0),
        ("a", "", 0),
        ("abc", "ABC", 1),
        ("abc", "xabc", 0),
    ];
    for (pattern, text, expected) in cases {
        assert_eq!(
            db.execute_params(
                "SELECT text_like(?, ?)",
                &[Value::Text(pattern.into()), Value::Text(text.into())],
            )
            .unwrap()[0]
                .rows,
            vec![vec![Value::Integer(expected)]],
            "pattern {pattern:?}, text {text:?}"
        );
    }
    assert_eq!(
        db.execute("SELECT text_like(NULL, 'a'), text_like('%', NULL), text_like(NULL, NULL)")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Null, Value::Null, Value::Null]]
    );
}

#[test]
fn sqlite_text_functions_preserve_embedded_nul_characters() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT hex(concat('a'||char(0)||'b','c')), hex(concat_ws(',', 'a'||char(0)||'b','c')), hex(ltrim(' a'||char(0)||'b')), hex(rtrim('a'||char(0)||'b '))")
            .unwrap()[0].rows,
        vec![vec![
            Value::Text("61006263".into()),
            Value::Text("6100622C63".into()),
            Value::Text("610062".into()),
            Value::Text("610062".into()),
        ]]
    );
}

#[test]
fn sqlite_octet_length_uses_database_encoding_while_text_size_uses_utf8() {
    let mut db = Database::memory();
    db.execute("PRAGMA encoding='UTF-16le'; CREATE TABLE strings(value TEXT); INSERT INTO strings VALUES('abc')").unwrap();
    assert_eq!(
        db.execute("SELECT octet_length(value),text_size(value) FROM strings")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(6), Value::Integer(3)]]
    );
}

#[test]
fn sqlean_text_functions_keep_their_implementations_without_sqlite_aliases() {
    let mut db = Database::memory();
    assert_eq!(
        db.execute("SELECT text_concat('a',NULL,'b'),text_join('/',NULL,'a','b'),text_ltrim('ёжик','ё'),text_rtrim('ёжик','к'),text_size('ёж')")
            .unwrap()[0].rows,
        vec![vec![
            Value::Text("ab".into()),
            Value::Text("a/b".into()),
            Value::Text("жик".into()),
            Value::Text("ёжи".into()),
            Value::Integer(4),
        ]]
    );
}

#[test]
fn regular_expressions_support_unicode_extraction_and_replacement() {
    let mut db = Database::memory();
    assert_eq!(db.execute("SELECT 'Привет' REGEXP '^\\w+$', regexp_substr('order #123', '\\d+'), regexp_replace('a1b2','\\d','_')").unwrap()[0].rows,
        vec![vec![Value::Integer(1), Value::Text("123".into()), Value::Text("a_b_".into())]]);
    assert!(db.execute("SELECT regexp_like('x','[')").is_err());
}

#[test]
fn vector_tables_find_nearest_rows_and_survive_binary_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vectors.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE VIRTUAL TABLE embeddings USING vec0(embedding float[3]); INSERT INTO embeddings(rowid,embedding) VALUES(1,'[1,0,0]'),(2,'[0,1,0]'),(3,'[0,0,1]')").unwrap();
    let query = "SELECT rowid,distance FROM embeddings WHERE embedding MATCH '[0.9,0,0]' AND k=1";
    let result = db.execute(query).unwrap();
    assert_eq!(result[0].rows[0][0], Value::Integer(1));
    let Value::Real(distance) = result[0].rows[0][1] else {
        panic!("expected vector distance")
    };
    assert!((distance - 0.1).abs() < 0.000001);
    assert!(
        db.execute("INSERT INTO embeddings VALUES(4,'[1,2]')")
            .is_err()
    );
    let snapshot = files::converted_bytes(&mut db, Format::Binary).unwrap();
    drop(db);
    let mut reopened = Database::open(&path, Format::Binary).unwrap();
    assert_eq!(reopened.execute(query).unwrap(), result);
    let mut restored = files::open_bytes(&snapshot).unwrap();
    assert_eq!(restored.execute(query).unwrap(), result);
    assert_eq!(
        restored
            .execute("SELECT vec_distance_cosine('[1,0,0]','[0,1,0]')")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Real(1.0)]]
    );
}

#[test]
fn extension_functions_and_collations_are_available_while_restoring_schema() {
    let directory = tempfile::tempdir().unwrap();
    for format in [Format::Binary, Format::Readable] {
        let path = directory.path().join(format!("{format:?}"));
        let mut db = Database::open(&path, format).unwrap();
        db.execute("CREATE TABLE terms(name TEXT COLLATE text_nocase CHECK(regexp_like(name,'^\\w+$')), amount DECIMAL(12,2), adjusted DECIMAL(12,2) GENERATED ALWAYS AS (decimal_round(amount+CAST('0.10' AS DECIMAL),2,'half_even')) STORED); CREATE INDEX folded ON terms(text_lower(name)); INSERT INTO terms(name,amount) VALUES('Ёж','0.20')").unwrap();
        let snapshot = files::converted_bytes(&mut db, format).unwrap();
        drop(db);
        for mut restored in [
            files::open(&path, format).unwrap(),
            files::open_bytes(&snapshot).unwrap(),
        ] {
            assert_eq!(
                restored
                    .execute("SELECT adjusted FROM terms WHERE name='ёж'")
                    .unwrap()[0]
                    .rows,
                vec![vec![Value::Decimal("0.30".into())]]
            );
        }
    }
}

#[test]
fn schema_file_exports_support_extension_functions_and_decimal_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.db");
    let mut db = Database::open(&path, Format::Binary).unwrap();
    db.execute("CREATE TABLE terms(name TEXT COLLATE text_nocase CHECK(regexp_like(name,'^\\w+$')), amount DECIMAL(12,2), adjusted DECIMAL(12,2) GENERATED ALWAYS AS (decimal_round(amount+CAST('0.10' AS DECIMAL),2,'half_even')) STORED); CREATE INDEX folded ON terms(text_lower(name))").unwrap();
    drop(db);
    let sql =
        String::from_utf8(Database::dump_schema_file(&path, SchemaFormat::Sql).unwrap()).unwrap();
    assert!(sql.contains("text_lower"));
    assert!(sql.contains("Decimal"));
    let yaml = Database::dump_schema_file(&path, SchemaFormat::Yaml).unwrap();
    let document: serde_json::Value = serde_saphyr::from_slice(&yaml).unwrap();
    assert_eq!(
        document["tables"]["terms"]["columns"]["amount"],
        "DECIMAL(12, 2)"
    );
    assert!(
        document["tables"]["terms"]["columns"]["adjusted"]
            .as_str()
            .unwrap()
            .contains("decimal_round")
    );
    assert_eq!(
        document["indexes"]["folded"]["expressions"],
        serde_json::json!(["text_lower(name)"])
    );
}

#[test]
fn full_text_search_supports_ranking_highlights_and_substrings() {
    let mut db = Database::memory();
    db.execute("CREATE VIRTUAL TABLE documents USING fts5(title,body); INSERT INTO documents(rowid,title,body) VALUES(1,'SQLite guide','A small database'),(2,'Other','A different engine')").unwrap();
    assert_eq!(db.execute("SELECT rowid,highlight(documents,0,'[',']') FROM documents WHERE documents MATCH 'sqlite' ORDER BY bm25(documents)").unwrap()[0].rows, vec![vec![Value::Integer(1), Value::Text("[SQLite] guide".into())]]);
    db.execute("CREATE VIRTUAL TABLE fragments USING fts5(body,tokenize='trigram'); INSERT INTO fragments VALUES('abcdefghij')").unwrap();
    assert_eq!(
        db.execute("SELECT body FROM fragments WHERE fragments MATCH 'cdefg'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Text("abcdefghij".into())]]
    );
    let snapshot = files::converted_bytes(&mut db, Format::Binary).unwrap();
    let mut restored = files::open_bytes(&snapshot).unwrap();
    assert_eq!(
        restored
            .execute("SELECT rowid FROM documents WHERE documents MATCH 'sqlite'")
            .unwrap()[0]
            .rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn spatial_indexes_find_overlapping_regions() {
    let mut db = Database::memory();
    db.execute("CREATE VIRTUAL TABLE regions USING rtree(id,min_x,max_x,min_y,max_y); INSERT INTO regions VALUES(1,10,20,30,40),(2,100,200,300,400)").unwrap();
    let query = "SELECT id FROM regions WHERE min_x<=15 AND max_x>=15 AND min_y<=35 AND max_y>=35";
    assert_eq!(
        db.execute(query).unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
    let snapshot = files::converted_bytes(&mut db, Format::Binary).unwrap();
    let mut restored = files::open_bytes(&snapshot).unwrap();
    assert_eq!(
        restored.execute(query).unwrap()[0].rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn unsupported_readable_virtual_tables_leave_the_connection_usable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search.yaml");
    let mut db = Database::open(&path, Format::Readable).unwrap();
    db.execute("CREATE TABLE notes(body TEXT)").unwrap();
    let previous = std::fs::read(&path).unwrap();
    assert!(
        db.execute("CREATE VIRTUAL TABLE search USING fts5(body)")
            .unwrap_err()
            .to_string()
            .contains("binary storage")
    );
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    db.execute("INSERT INTO notes VALUES('still usable')")
        .unwrap();
    drop(db);
    let mut reopened = Database::open(&path, Format::Readable).unwrap();
    assert_eq!(
        reopened.execute("SELECT body FROM notes").unwrap()[0].rows,
        vec![vec![Value::Text("still usable".into())]]
    );
}
