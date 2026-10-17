package boxdb_test

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

func TestCorruptConnectionsAreDiscardedByThePool(t *testing.T) {
	path := filepath.Join(t.TempDir(), "data.db")
	db, err := boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)"); err != nil {
		t.Fatal(err)
	}
	var root, pageSize int64
	if err := db.QueryRow("SELECT rootpage FROM sqlite_schema WHERE name='items'").Scan(&root); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("PRAGMA page_size").Scan(&pageSize); err != nil {
		t.Fatal(err)
	}
	db.Close()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	data[(root-1)*pageSize] = 255
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	db, err = boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	var id int64
	err = db.QueryRow("SELECT id FROM items").Scan(&id)
	var failure *boxdb.Error
	if !errors.As(err, &failure) || failure.Code != "BOXDB_CORRUPT" || failure.ConnectionUsable == nil || *failure.ConnectionUsable {
		t.Fatalf("unexpected error: %+v", err)
	}
	if db.Stats().OpenConnections != 0 {
		t.Fatal("corrupt connection was returned to pool")
	}
}

func TestFileMaintenancePreservesSource(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "data.db")
	db, err := boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1)"); err != nil {
		t.Fatal(err)
	}
	db.Close()
	before, _ := os.ReadFile(path)
	report, err := boxdb.CheckFile(path, boxdb.CheckFull)
	if err != nil || report.Status != boxdb.CheckValid || !report.Complete {
		t.Fatalf("%+v: %v", report, err)
	}
	repaired, err := boxdb.RepairFile(path, filepath.Join(directory, "repaired.db"))
	if err != nil || repaired.RecoveredRows != 1 {
		t.Fatalf("%+v: %v", repaired, err)
	}
	recovered, err := boxdb.RecoverFile(path, filepath.Join(directory, "recovered.db"), boxdb.Binary)
	if err != nil || !recovered.LossPossible {
		t.Fatalf("%+v: %v", recovered, err)
	}
	after, _ := os.ReadFile(path)
	if string(before) != string(after) {
		t.Fatal("source changed")
	}
}
