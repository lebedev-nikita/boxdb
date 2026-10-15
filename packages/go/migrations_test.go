package boxdb_test

import (
	"context"
	"errors"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

var migrationFiles = []boxdb.MigrationFile{{
	Filename: "20261015000000_create_events.sql",
	SQL:      "-- migrate:up\nCREATE TABLE events(at INSTANT);\n-- migrate:down\nDROP TABLE events;\n",
}}

func TestEmbeddedMigrationsApplyOnceAndRollBack(t *testing.T) {
	migrations, err := boxdb.NewMigrationSet(migrationFiles)
	if err != nil {
		t.Fatal(err)
	}
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	ctx := context.Background()
	want := []boxdb.MigrationInfo{{Version: "20261015000000", Name: "create_events"}}
	applied, err := migrations.Up(ctx, db)
	if err != nil || !reflect.DeepEqual(applied, want) {
		t.Fatalf("up = %v, %v", applied, err)
	}
	applied, err = migrations.Up(ctx, db)
	if err != nil || len(applied) != 0 {
		t.Fatalf("repeated up = %v, %v", applied, err)
	}
	if _, err := db.Exec("INSERT INTO events VALUES('2024-01-01T00:00:00Z')"); err != nil {
		t.Fatal(err)
	}
	var at string
	if err := db.QueryRow("SELECT at FROM events").Scan(&at); err != nil || at != "2024-01-01T00:00:00.000000Z" {
		t.Fatalf("typed row = %s, %v", at, err)
	}
	status, err := migrations.Status(ctx, db)
	if err != nil || len(status) != 1 || status[0].State != boxdb.MigrationApplied {
		t.Fatalf("status = %v, %v", status, err)
	}
	down, err := migrations.Down(ctx, db)
	if err != nil || down == nil || *down != want[0] {
		t.Fatalf("down = %v, %v", down, err)
	}
	down, err = migrations.Down(ctx, db)
	if err != nil || down != nil {
		t.Fatalf("empty down = %v, %v", down, err)
	}
}

func TestFilesystemMigrationsShareEmbeddedHistory(t *testing.T) {
	files := fstest.MapFS{
		"db/migrations/20261015000000_create_events.sql": &fstest.MapFile{Data: []byte(migrationFiles[0].SQL)},
		"db/migrations/README.txt":                       &fstest.MapFile{Data: []byte("ignored")},
	}
	migrations, err := boxdb.ReadMigrations(files, "db/migrations")
	if err != nil {
		t.Fatal(err)
	}
	embedded, err := boxdb.NewMigrationSet([]boxdb.MigrationFile{{Filename: migrationFiles[0].Filename, SQL: strings.ReplaceAll(migrationFiles[0].SQL, "\n", "\r\n")}})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	for _, format := range []boxdb.Format{boxdb.Binary, boxdb.Readable} {
		t.Run(string(format), func(t *testing.T) {
			db, err := boxdb.Open(filepath.Join(t.TempDir(), "data.db"), boxdb.WithFormat(format))
			if err != nil {
				t.Fatal(err)
			}
			defer db.Close()
			if _, err := migrations.Up(ctx, db); err != nil {
				t.Fatal(err)
			}
			applied, err := embedded.Up(ctx, db)
			if err != nil || len(applied) != 0 {
				t.Fatalf("embedded up = %v, %v", applied, err)
			}
			if _, err := embedded.Down(ctx, db); err != nil {
				t.Fatal(err)
			}
			statuses, err := migrations.Status(ctx, db)
			if err != nil || len(statuses) != 1 || statuses[0].State != boxdb.MigrationPending {
				t.Fatalf("status = %v, %v", statuses, err)
			}
		})
	}
}

func TestMigrationCancellationRollsBackAndRetainsEarlierVersions(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	initial, err := boxdb.NewMigrationSet(migrationFiles)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := initial.Up(context.Background(), db); err != nil {
		t.Fatal(err)
	}
	migrations, err := boxdb.NewMigrationSet(append(append([]boxdb.MigrationFile{}, migrationFiles...), boxdb.MigrationFile{
		Filename: "20261015000001_insert_events.sql",
		SQL:      "-- migrate:up\n" + strings.Repeat("INSERT INTO events VALUES('2024-01-01T00:00:00Z');\n", 5000) + "-- migrate:down\nDELETE FROM events;\n",
	}))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := migrations.Up(ctx, db); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("cancellation = %v", err)
	}
	statuses, err := migrations.Status(context.Background(), db)
	if err != nil || len(statuses) != 2 || statuses[0].State != boxdb.MigrationApplied || statuses[1].State != boxdb.MigrationPending {
		t.Fatalf("status = %v, %v", statuses, err)
	}
	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM events").Scan(&count); err != nil || count != 0 {
		t.Fatalf("count = %d, %v", count, err)
	}
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
}

func TestMigrationsPreserveStructuredErrorsAndCopyDocuments(t *testing.T) {
	_, err := boxdb.NewMigrationSet(append(append([]boxdb.MigrationFile{}, migrationFiles...), migrationFiles[0]))
	var failure *boxdb.Error
	if !errors.As(err, &failure) || failure.Code != "BOXDB_VALIDATION" {
		t.Fatalf("duplicate validation = %v", err)
	}
	files := append([]boxdb.MigrationFile{}, migrationFiles...)
	migrations, err := boxdb.NewMigrationSet(files)
	if err != nil {
		t.Fatal(err)
	}
	files[0].SQL = "invalid"
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	ctx := context.Background()
	if _, err := migrations.Up(ctx, db); err != nil {
		t.Fatal(err)
	}
	failed, err := boxdb.NewMigrationSet(append(append([]boxdb.MigrationFile{}, migrationFiles...), boxdb.MigrationFile{
		Filename: "20261015000001_duplicate.sql",
		SQL:      "-- migrate:up\nCREATE TABLE names(name TEXT UNIQUE); INSERT INTO names VALUES('Ada'); INSERT INTO names VALUES('Ada');\n-- migrate:down\nDROP TABLE names;",
	}))
	if err != nil {
		t.Fatal(err)
	}
	_, err = failed.Up(ctx, db)
	if !errors.As(err, &failure) || failure.Code != "BOXDB_CONSTRAINT_UNIQUE" || failure.ExtendedCode == nil || *failure.ExtendedCode != 2067 || !strings.Contains(failure.Message, "20261015000001 up") {
		t.Fatalf("failed migration = %v", err)
	}
	statuses, err := failed.Status(ctx, db)
	if err != nil || len(statuses) != 2 || statuses[0].State != boxdb.MigrationApplied || statuses[1].State != boxdb.MigrationPending {
		t.Fatalf("status = %v, %v", statuses, err)
	}
	if _, err := db.Exec("SELECT * FROM names"); err == nil {
		t.Fatal("failed migration's table survived rollback")
	}
}

func TestConcurrentMigrationsPinPooledConnections(t *testing.T) {
	db, err := boxdb.Open(filepath.Join(t.TempDir(), "data.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(4)
	migrations, err := boxdb.NewMigrationSet(append(append([]boxdb.MigrationFile{}, migrationFiles...), boxdb.MigrationFile{
		Filename: "20261015000001_insert_events.sql",
		SQL:      "-- migrate:up\nWITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO events SELECT '2024-01-01T00:00:00Z' FROM n;\n-- migrate:down\nDELETE FROM events;",
	}))
	if err != nil {
		t.Fatal(err)
	}
	results := make(chan int, 2)
	var group sync.WaitGroup
	for range 2 {
		group.Add(1)
		go func() {
			defer group.Done()
			applied, err := migrations.Up(context.Background(), db)
			if err != nil {
				t.Error(err)
			}
			results <- len(applied)
		}()
	}
	group.Wait()
	if total := <-results + <-results; total != 2 {
		t.Fatalf("applied %d versions", total)
	}
	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM events").Scan(&count); err != nil || count != 1000 {
		t.Fatalf("count = %d, %v", count, err)
	}
}

func TestMigrationCancellationWhileWaitingForPoolOrWriter(t *testing.T) {
	for _, pooled := range []bool{false, true} {
		t.Run(map[bool]string{false: "writer", true: "pool"}[pooled], func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "data.db")
			holder, err := boxdb.Open(path)
			if err != nil {
				t.Fatal(err)
			}
			defer holder.Close()
			db := holder
			if !pooled {
				db, err = boxdb.Open(path)
				if err != nil {
					t.Fatal(err)
				}
				defer db.Close()
			}
			tx, err := holder.Begin()
			if err != nil {
				t.Fatal(err)
			}
			defer tx.Rollback()
			migrations, err := boxdb.NewMigrationSet(migrationFiles)
			if err != nil {
				t.Fatal(err)
			}
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
			defer cancel()
			if _, err := migrations.Up(ctx, db); !errors.Is(err, context.DeadlineExceeded) {
				t.Fatalf("waiting cancellation = %v", err)
			}
			if err := tx.Rollback(); err != nil {
				t.Fatal(err)
			}
			if _, err := migrations.Up(context.Background(), db); err != nil {
				t.Fatal(err)
			}
		})
	}
}
