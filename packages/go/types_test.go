package boxdb_test

import (
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"testing"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

func TestEnumColumnsBindAndScanStrings(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE tasks(status ENUM('todo','it''s Done') NOT NULL DEFAULT 'todo')"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO tasks VALUES(?)", "it's Done"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("UPDATE tasks SET status=?", "done"); err == nil {
		t.Fatal("invalid enum label was accepted")
	}
	if _, err := db.Exec("ALTER TABLE tasks ADD COLUMN priority ENUM('low','high') DEFAULT 'low'"); err != nil {
		t.Fatal(err)
	}
	var status, priority string
	if err := db.QueryRow("SELECT status,priority FROM tasks").Scan(&status, &priority); err != nil {
		t.Fatal(err)
	}
	if status != "it's Done" || priority != "low" {
		t.Fatalf("unexpected enum strings: %q, %q", status, priority)
	}
}

func TestDecimalCodecPreservesExactAmountsAndNulls(t *testing.T) {
	var _ sql.Scanner = (*boxdb.Decimal)(nil)
	var _ driver.Valuer = boxdb.Decimal("")
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE amounts(v DECIMAL(20,2))"); err != nil {
		t.Fatal(err)
	}
	amount := boxdb.Decimal("9007199254740993.01")
	if _, err := db.Exec("INSERT INTO amounts VALUES(?),(NULL)", amount); err != nil {
		t.Fatal(err)
	}
	var got boxdb.Decimal
	if err := db.QueryRow("SELECT v+CAST('0.02' AS DECIMAL) FROM amounts WHERE v=?", amount).Scan(&got); err != nil {
		t.Fatal(err)
	}
	if got != "9007199254740993.03" {
		t.Fatalf("amount = %q", got)
	}
	var nullable sql.Null[boxdb.Decimal]
	if err := db.QueryRow("SELECT v FROM amounts WHERE v IS NULL").Scan(&nullable); err != nil {
		t.Fatal(err)
	}
	if nullable.Valid {
		t.Fatal("SQL NULL became a decimal value")
	}
	if _, err := db.Exec("INSERT INTO amounts VALUES(?)", boxdb.Decimal("1.001")); err == nil {
		t.Fatal("invalid scale was accepted")
	}
}

func TestNativeTextCodecsPreserveCanonicalValuesAndMicroseconds(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE events(id UUID, at INSTANT, day DATE, local DATETIME, duration INTERVAL)"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO events VALUES(?,?,?,?,?)",
		boxdb.UUID("550E8400E29B41D4A716446655440000"),
		boxdb.Instant("2024-02-29T16:00:00.123456789+04:00"),
		boxdb.Date("2024-02-29"), boxdb.DateTime("2024-02-29T12:00:00.123456789"),
		boxdb.Interval("1 month 2 days 3 hours")); err != nil {
		t.Fatal(err)
	}
	var id boxdb.UUID
	var at boxdb.Instant
	var day boxdb.Date
	var local boxdb.DateTime
	var duration boxdb.Interval
	if err := db.QueryRow("SELECT * FROM events").Scan(&id, &at, &day, &local, &duration); err != nil {
		t.Fatal(err)
	}
	if id != "550e8400-e29b-41d4-a716-446655440000" || at != "2024-02-29T12:00:00.123457Z" || day != "2024-02-29" || local != "2024-02-29T12:00:00.123457" || duration != "1 mons 2 days 03:00:00.000000" {
		t.Fatalf("unexpected values: %s %s %s %s %s", id, at, day, local, duration)
	}
	for _, scanner := range []sql.Scanner{&id, &at, &day, &local, &duration} {
		if err := scanner.Scan(nil); err == nil {
			t.Fatal("SQL NULL was accepted without sql.Null[T]")
		}
		if err := scanner.Scan(float64(1.25)); err == nil {
			t.Fatal("floating-point value was converted to native text")
		}
	}
	if at != "2024-02-29T12:00:00.123457Z" {
		t.Fatal("failed Scan changed the previous value")
	}
	if _, err := db.Exec("INSERT INTO events(day) VALUES(?)", boxdb.Date("2024-02-30")); err == nil {
		t.Fatal("invalid date was accepted")
	}
}

func TestJSONCodecKeepsDocumentsNumbersAndSQLNullDistinct(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE documents(body JSON)"); err != nil {
		t.Fatal(err)
	}
	document := boxdb.JSON[map[string]any]{Data: map[string]any{"count": json.Number("9007199254740993"), "theme": "dark"}}
	if _, err := db.Exec("INSERT INTO documents VALUES(?),(?), (NULL)", document, boxdb.JSON[any]{Data: nil}); err != nil {
		t.Fatal(err)
	}
	var got boxdb.JSON[map[string]any]
	if err := db.QueryRow("SELECT body FROM documents WHERE rowid=1").Scan(&got); err != nil {
		t.Fatal(err)
	}
	if got.Data["count"] != json.Number("9007199254740993") || got.Data["theme"] != "dark" {
		t.Fatalf("document = %#v", got.Data)
	}
	encoded, err := json.Marshal(got)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) != `{"count":9007199254740993,"theme":"dark"}` {
		t.Fatalf("encoded = %s", encoded)
	}
	var decoded boxdb.JSON[map[string]any]
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Data["count"] != json.Number("9007199254740993") {
		t.Fatalf("decoded = %#v", decoded.Data)
	}
	var jsonNull boxdb.JSON[any]
	if err := db.QueryRow("SELECT body FROM documents WHERE rowid=2").Scan(&jsonNull); err != nil {
		t.Fatal(err)
	}
	if jsonNull.Data != nil {
		t.Fatal("JSON null was changed")
	}
	var sqlNull sql.Null[boxdb.JSON[any]]
	if err := db.QueryRow("SELECT body FROM documents WHERE rowid=3").Scan(&sqlNull); err != nil {
		t.Fatal(err)
	}
	if sqlNull.Valid {
		t.Fatal("SQL NULL became a JSON document")
	}
	for _, invalid := range []any{nil, `{"count":`, `{} {}`} {
		if err := got.Scan(invalid); err == nil {
			t.Fatalf("accepted invalid document: %v", invalid)
		}
		if got.Data["count"] != json.Number("9007199254740993") {
			t.Fatal("failed Scan changed the previous document")
		}
	}
}
