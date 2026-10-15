package boxdb_test

import (
	"context"
	"os"
	"testing"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

// The Rust integration runner coordinates both clients, CLI checks and conversion.
func TestCrossLanguageNativeTypes(t *testing.T) {
	path := os.Getenv("BOXDB_CLIENT_TYPES_DB")
	if path == "" {
		t.Skip("run just client-types-test")
	}
	phase := os.Getenv("BOXDB_CLIENT_TYPES_PHASE")
	if phase != "seed" && phase != "verify" {
		t.Fatalf("unknown phase %q", phase)
	}
	db, err := boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if phase == "seed" {
		migrations, err := boxdb.ReadMigrations(os.DirFS(os.Getenv("BOXDB_CLIENT_TYPES_MIGRATIONS")), ".")
		if err != nil {
			t.Fatal(err)
		}
		if _, err := migrations.Up(context.Background(), db); err != nil {
			t.Fatal(err)
		}
		_, err = db.Exec("INSERT INTO typed(id,amount,external,occurred,day,local,duration,document) VALUES(1,?,?,?,?,?,?,?)",
			boxdb.Decimal("9007199254740993.01"), boxdb.UUID("550E8400E29B41D4A716446655440000"),
			boxdb.Instant("2024-02-29T16:00:00.123456789+04:00"), boxdb.Date("2024-02-29"),
			boxdb.DateTime("2024-02-29T12:00:00.123456789"), boxdb.Interval("1 month"),
			boxdb.JSON[map[string]string]{Data: map[string]string{"ref": "9007199254740993", "source": "go"}})
		if err != nil {
			t.Fatal(err)
		}
	}
	var amount, calculated boxdb.Decimal
	var id boxdb.UUID
	var at boxdb.Instant
	var day boxdb.Date
	var local boxdb.DateTime
	var duration boxdb.Interval
	var document boxdb.JSON[map[string]string]
	if err := db.QueryRow("SELECT amount,calculated,external,occurred,day,local,duration,document FROM typed WHERE id=1").Scan(&amount, &calculated, &id, &at, &day, &local, &duration, &document); err != nil {
		t.Fatal(err)
	}
	source := "go"
	if phase == "verify" {
		source = "typescript"
	}
	if amount != "9007199254740993.01" || calculated != "9007199254740993.02" || id != "550e8400-e29b-41d4-a716-446655440000" ||
		at != "2024-02-29T12:00:00.123457Z" || day != "2024-02-29" || local != "2024-02-29T12:00:00.123457" || duration != "1 mons 0 days 00:00:00.000000" ||
		document.Data["ref"] != "9007199254740993" || document.Data["source"] != source {
		t.Fatalf("native values changed: %s %s %s %s %s %s %s %#v", amount, calculated, id, at, day, local, duration, document.Data)
	}
	if phase == "verify" {
		var count int
		if err := db.QueryRow("SELECT COUNT(*) FROM typed WHERE label='migrated'").Scan(&count); err != nil {
			t.Fatal(err)
		}
		if count != 3 {
			t.Fatalf("expected three migrated rows, got %d", count)
		}
	}
}
