package boxdb_test

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"testing"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

func TestCommitTriggersRecordChangesetsThatUndoTransactions(t *testing.T) {
	var _ sql.Scanner = (*boxdb.Changeset)(nil)
	var _ driver.Valuer = boxdb.Changeset(nil)
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	ctx := context.Background()
	if _, err := db.ExecContext(ctx, `
		CREATE TABLE tasks(id INT64 PRIMARY KEY, title TEXT NOT NULL);
		CREATE TABLE history(id INT64 PRIMARY KEY, label TEXT, changes CHANGESET NOT NULL);
		CREATE TRIGGER record_history BEFORE COMMIT ON tasks BEGIN
		  INSERT INTO history(label, changes) VALUES(boxdb_context('label'), boxdb_changeset());
		END;
		INSERT INTO tasks VALUES(1,'a')`); err != nil {
		t.Fatal(err)
	}
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.ExecContext(ctx, "SELECT boxdb_set_context('label', ?)", "rename"); err != nil {
		t.Fatal(err)
	}
	if _, err := tx.ExecContext(ctx, "UPDATE tasks SET title=? WHERE id=?", "b", 1); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	var label string
	var changes boxdb.Changeset
	if err := db.QueryRowContext(ctx, "SELECT label, changes FROM history WHERE id=2").Scan(&label, &changes); err != nil {
		t.Fatal(err)
	}
	if label != "rename" || len(changes) == 0 {
		t.Fatalf("unexpected history row: %q, %d bytes", label, len(changes))
	}

	tx, err = db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	var report string
	if err := tx.QueryRowContext(ctx, "SELECT boxdb_apply(?, ?)", changes, `{"invert":true}`).Scan(&report); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	var decoded struct {
		Applied   int   `json:"applied"`
		Skipped   int   `json:"skipped"`
		Conflicts []any `json:"conflicts"`
	}
	if err := json.Unmarshal([]byte(report), &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Applied != 1 || decoded.Skipped != 0 || len(decoded.Conflicts) != 0 {
		t.Fatalf("unexpected report %s", report)
	}
	var title string
	var count int
	if err := db.QueryRowContext(ctx, "SELECT title, (SELECT count(*) FROM history) FROM tasks").Scan(&title, &count); err != nil {
		t.Fatal(err)
	}
	if title != "a" || count != 3 {
		t.Fatalf("undo not applied or not recorded: %q, %d", title, count)
	}

	// A conflicting undo fails inside the transaction and leaves rows alone.
	if _, err := db.ExecContext(ctx, "UPDATE tasks SET title='edited'"); err != nil {
		t.Fatal(err)
	}
	tx, err = db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	err = tx.QueryRowContext(ctx, "SELECT boxdb_apply(?, ?)", changes, `{"invert":true}`).Scan(&report)
	var failure *boxdb.Error
	if !errors.As(err, &failure) || failure.Code != "BOXDB_CONSTRAINT_FUNCTION" {
		t.Fatalf("expected a conflict error, got %v", err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
	var fields int
	if err := db.QueryRowContext(ctx, "SELECT count(*) FROM changeset_rows(?)", changes).Scan(&fields); err != nil {
		t.Fatal(err)
	}
	if fields != 2 {
		t.Fatalf("expected two changed fields, got %d", fields)
	}
	if _, err := db.ExecContext(ctx, "INSERT INTO history(label, changes) VALUES(?, ?)", "manual", changes); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ExecContext(ctx, "INSERT INTO history(label, changes) VALUES(?, ?)", "broken", []byte("garbage")); err == nil {
		t.Fatal("invalid changeset bytes were accepted")
	}
}
