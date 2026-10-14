use boxdb::{Database, Value};

#[test]
fn json_is_validated_and_stored_without_sql_document_operations() {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE documents(body JSON); INSERT INTO documents VALUES('{\"enabled\":true}')",
    )
    .unwrap();
    assert!(
        db.execute("INSERT INTO documents VALUES('{broken')")
            .is_err()
    );
    assert_eq!(
        db.execute("SELECT body FROM documents").unwrap()[0].rows,
        vec![vec![Value::Json(serde_json::json!({"enabled": true}))]]
    );
    assert!(
        db.execute("SELECT json_extract(body,'$.enabled') FROM documents")
            .is_err()
    );
}
