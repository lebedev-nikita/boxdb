package boxdb_test

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
	"time"

	boxdb "github.com/lebedev-nikita/boxdb/packages/go"
)

func TestQueriesThroughDatabaseSQL(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	ctx := context.Background()
	if _, err := db.ExecContext(ctx, "CREATE TABLE users(id INT64 PRIMARY KEY, name TEXT NOT NULL)"); err != nil {
		t.Fatal(err)
	}
	result, err := db.ExecContext(ctx, "INSERT INTO users(name) VALUES (?)", "Ada")
	if err != nil {
		t.Fatal(err)
	}
	id, err := result.LastInsertId()
	if err != nil || id != 1 {
		t.Fatalf("insert id = %d, error = %v", id, err)
	}
	count, err := result.RowsAffected()
	if err != nil || count != 1 {
		t.Fatalf("affected = %d, error = %v", count, err)
	}
	var name string
	if err := db.QueryRowContext(ctx, "SELECT name FROM users WHERE id = ?", id).Scan(&name); err != nil {
		t.Fatal(err)
	}
	if name != "Ada" {
		t.Fatalf("name = %q", name)
	}
}

func TestRegisteredDriverSupportsConcurrentCallsAndSQLValues(t *testing.T) {
	db, err := sql.Open("boxdb", ":memory:")
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE users(id INT64, name TEXT)"); err != nil {
		t.Fatal(err)
	}
	var workers sync.WaitGroup
	failures := make(chan error, 16)
	for i := 0; i < 16; i++ {
		workers.Add(1)
		go func(id int) {
			defer workers.Done()
			_, err := db.Exec("INSERT INTO users VALUES(?,?)", id, sql.NullString{String: "Ada", Valid: true})
			if err != nil {
				failures <- err
			}
		}(i)
	}
	workers.Wait()
	close(failures)
	for err := range failures {
		t.Fatal(err)
	}
	var count int
	if err := db.QueryRow("SELECT count(*) FROM users WHERE name='Ada'").Scan(&count); err != nil || count != 16 {
		t.Fatalf("concurrent inserts = %d, %v", count, err)
	}
	var null sql.NullString
	var empty []byte
	var text string
	if err := db.QueryRow("SELECT ?, ?, ?", sql.NullString{}, []byte{}, "before\x00after").Scan(&null, &empty, &text); err != nil {
		t.Fatal(err)
	}
	if null.Valid || empty == nil || len(empty) != 0 || text != "before\x00after" {
		t.Fatalf("SQL values = %v, %v, %q", null, empty, text)
	}
}

func TestShortDeadlinesPreserveInMemoryData(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec("CREATE TABLE users(name TEXT); INSERT INTO users VALUES('Ada')"); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 100; i++ {
		ctx, cancel := context.WithTimeout(context.Background(), 50*time.Microsecond)
		err := db.QueryRowContext(ctx, "SELECT name FROM users").Scan(new(string))
		cancel()
		if err != nil && !errors.Is(err, context.DeadlineExceeded) && !errors.Is(err, context.Canceled) {
			t.Fatal(err)
		}
	}
	var name string
	if err := db.QueryRow("SELECT name FROM users").Scan(&name); err != nil || name != "Ada" {
		t.Fatalf("data after cancellations = %q, %v", name, err)
	}
}

func TestExecScriptsAndParameterizedStatementBoundaries(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	_, err = db.Exec("CREATE TABLE users(id INT64 PRIMARY KEY, name TEXT); INSERT INTO users(name) VALUES('Ada'); INSERT INTO users(name) VALUES('Grace');")
	if err != nil {
		t.Fatal(err)
	}
	var count int
	if err := db.QueryRow("SELECT count(*) FROM users").Scan(&count); err != nil || count != 2 {
		t.Fatalf("script rows = %d, %v", count, err)
	}
	if _, err := db.Exec("INSERT INTO users(name) VALUES(?); DELETE FROM users", "Linus"); err == nil {
		t.Fatal("parameterized script was accepted")
	}
	if _, err := db.Query("SELECT 1; SELECT 2"); err == nil {
		t.Fatal("query accepted multiple statements")
	}
	if err := db.QueryRow("SELECT count(*) FROM users").Scan(&count); err != nil || count != 2 {
		t.Fatalf("rejected script changed rows = %d, %v", count, err)
	}
}

func TestFailedCommitAndCancelledTransactionReleaseConnection(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	for _, query := range []string{"CREATE TABLE parents(id INT64 PRIMARY KEY)", "CREATE TABLE children(parent_id INT64 REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)"} {
		if _, err := db.Exec(query); err != nil {
			t.Fatal(err)
		}
	}
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("INSERT INTO children VALUES(99)"); err != nil {
		tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err == nil {
		t.Fatal("commit ignored deferred foreign key violation")
	}
	var count int
	if err := db.QueryRow("SELECT count(*) FROM children").Scan(&count); err != nil || count != 0 {
		t.Fatalf("after failed commit = %d, %v", count, err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	tx, err = db.BeginTx(ctx, nil)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	if _, err := tx.Exec("INSERT INTO parents VALUES(1)"); err != nil {
		cancel()
		tx.Rollback()
		t.Fatal(err)
	}
	cancel()
	ctx, stop := context.WithTimeout(context.Background(), time.Second)
	defer stop()
	if err := db.QueryRowContext(ctx, "SELECT count(*) FROM parents").Scan(&count); err != nil || count != 0 {
		t.Fatalf("after cancelled transaction = %d, %v", count, err)
	}
}

func TestContextCancellationStopsWaitingForWriterLock(t *testing.T) {
	path := filepath.Join(t.TempDir(), "locked.db")
	writer, err := boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()
	if _, err := writer.Exec("CREATE TABLE users(name TEXT)"); err != nil {
		t.Fatal(err)
	}
	waiter, err := boxdb.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer waiter.Close()
	tx, err := writer.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer tx.Rollback()
	if _, err := tx.Exec("INSERT INTO users VALUES('Ada')"); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	start := time.Now()
	_, err = waiter.ExecContext(ctx, "INSERT INTO users VALUES('Grace')")
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("lock cancellation = %v", err)
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Fatalf("cancellation took %s", elapsed)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	if _, err := waiter.Exec("INSERT INTO users VALUES('Grace')"); err != nil {
		t.Fatalf("connection after lock cancellation: %v", err)
	}
}

func TestContextCancellationInterruptsQueriesAndKeepsConnectionUsable(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	var sum int64
	err = db.QueryRowContext(ctx, "WITH RECURSIVE numbers(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM numbers WHERE x<1000000000) SELECT sum(x) FROM numbers").Scan(&sum)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("query cancellation = %v", err)
	}
	if err := db.QueryRow("SELECT 42").Scan(&sum); err != nil || sum != 42 {
		t.Fatalf("connection after cancellation = %d, %v", sum, err)
	}
	ctx, cancel = context.WithCancel(context.Background())
	cancel()
	if _, err := db.ExecContext(ctx, "CREATE TABLE cancelled(id INT64)"); !errors.Is(err, context.Canceled) {
		t.Fatalf("already canceled execution = %v", err)
	}
}

func TestStorageFormatsReopenAndClose(t *testing.T) {
	for _, format := range []boxdb.Format{boxdb.Binary, boxdb.Readable} {
		t.Run(string(format), func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "data.db")
			db, err := boxdb.Open(path, boxdb.WithFormat(format))
			if err != nil {
				t.Fatal(err)
			}
			if _, err := db.Exec("CREATE TABLE accounts(balance DECIMAL(12,2))"); err != nil {
				db.Close()
				t.Fatal(err)
			}
			if _, err := db.Exec("INSERT INTO accounts VALUES(?)", "12.50"); err != nil {
				db.Close()
				t.Fatal(err)
			}
			if err := db.Close(); err != nil {
				t.Fatal(err)
			}
			if err := db.Ping(); err == nil {
				t.Fatal("closed database is usable")
			}
			contents, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			binary := bytes.HasPrefix(contents, []byte("SQLite format 3\x00"))
			if binary != (format == boxdb.Binary) {
				t.Fatalf("unexpected file format: %s", contents[:16])
			}
			db, err = boxdb.Open(path)
			if err != nil {
				t.Fatal(err)
			}
			defer db.Close()
			var balance string
			if err := db.QueryRow("SELECT balance FROM accounts").Scan(&balance); err != nil {
				t.Fatal(err)
			}
			if balance != "12.50" {
				t.Fatalf("balance = %q", balance)
			}
		})
	}
	if db, err := boxdb.Open(":memory:", boxdb.WithFormat("invalid")); err == nil {
		db.Close()
		t.Fatal("invalid format was accepted")
	}
	if db, err := boxdb.Open("bad\x00path"); err == nil {
		db.Close()
		t.Fatal("NUL path was accepted")
	}
}

func TestTransactionsCommitRollbackAndReadOnly(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	if _, err := db.Exec("CREATE TABLE users(name TEXT)"); err != nil {
		t.Fatal(err)
	}
	for _, commit := range []bool{false, true} {
		tx, err := db.BeginTx(context.Background(), &sql.TxOptions{Isolation: sql.LevelSerializable})
		if err != nil {
			t.Fatal(err)
		}
		stmt, err := tx.Prepare("INSERT INTO users VALUES(?)")
		if err != nil {
			tx.Rollback()
			t.Fatal(err)
		}
		_, err = stmt.Exec("Ada")
		stmt.Close()
		if err != nil {
			tx.Rollback()
			t.Fatal(err)
		}
		if commit {
			err = tx.Commit()
		} else {
			err = tx.Rollback()
		}
		if err != nil {
			t.Fatal(err)
		}
	}
	var count int
	if err := db.QueryRow("SELECT count(*) FROM users").Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("committed rows = %d", count)
	}
	tx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec("INSERT INTO users VALUES('Grace')"); err == nil {
		tx.Rollback()
		t.Fatal("read-only transaction accepted a write")
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO users VALUES('Grace')"); err != nil {
		t.Fatalf("read-only state leaked: %v", err)
	}
	if _, err := db.BeginTx(context.Background(), &sql.TxOptions{Isolation: sql.LevelReadCommitted}); err == nil {
		t.Fatal("unsupported isolation was accepted")
	}
}

func TestBoxDBValuesAndValidation(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	_, err = db.Exec("CREATE TABLE values_test(id INT64, amount DECIMAL(12,2), enabled BOOLEAN, payload BYTEA, nullable TEXT, at INSTANT, data JSON, tags INT8[])")
	if err != nil {
		t.Fatal(err)
	}
	at := time.Date(2026, 10, 14, 12, 30, 0, 123456789, time.FixedZone("+04", 4*60*60))
	_, err = db.Exec("INSERT INTO values_test VALUES(?,?,?,?,?,?,?,?)", int64(math.MaxInt64), "12.50", true, []byte{0, 255, 128}, nil, at, `{"name":"Ada"}`, `[1,2]`)
	if err != nil {
		t.Fatal(err)
	}
	var id int64
	var amount, instant, data, tags string
	var enabled bool
	var payload []byte
	var nullable sql.NullString
	if err := db.QueryRow("SELECT * FROM values_test").Scan(&id, &amount, &enabled, &payload, &nullable, &instant, &data, &tags); err != nil {
		t.Fatal(err)
	}
	if id != math.MaxInt64 || amount != "12.50" || !enabled || !bytes.Equal(payload, []byte{0, 255, 128}) || nullable.Valid || instant != "2026-10-14T08:30:00.123456789Z" || data != `{"name":"Ada"}` || tags != "[1,2]" {
		t.Fatalf("unexpected values: %d %s %v %v %v %s %s %s", id, amount, enabled, payload, nullable, instant, data, tags)
	}
	_, err = db.Exec("INSERT INTO values_test(tags) VALUES(?)", "[128]")
	var coreError *boxdb.Error
	if !errors.As(err, &coreError) {
		t.Fatalf("validation error = %v", err)
	}
	var decimal, lower string
	if err := db.QueryRow("SELECT decimal_add('0.1','0.2'), text_lower('ПРИВЕТ')").Scan(&decimal, &lower); err != nil {
		t.Fatal(err)
	}
	if decimal != "0.3" || lower != "привет" {
		t.Fatalf("extensions = %q, %q", decimal, lower)
	}
}

func TestPreparedStatementsAndNamedParameters(t *testing.T) {
	db, err := boxdb.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { db.Close() })
	stmt, err := db.PrepareContext(context.Background(), "SELECT :name AS name, :id AS id, :name AS again")
	if err != nil {
		t.Fatal(err)
	}
	defer stmt.Close()
	for _, name := range []string{"Ada", "Grace"} {
		var got, again string
		var id int64
		if err := stmt.QueryRow(sql.Named("id", 42), sql.Named("name", name)).Scan(&got, &id, &again); err != nil {
			t.Fatal(err)
		}
		if got != name || again != name || id != 42 {
			t.Fatalf("row = %q, %d, %q", got, id, again)
		}
	}
	if err := db.QueryRow("SELECT @name, $name", sql.Named("name", "Linus")).Scan(new(string), new(string)); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT :name", sql.Named("typo", "Ada")).Scan(new(string)); err == nil {
		t.Fatal("unknown parameter was accepted")
	}
	if err := db.QueryRow("SELECT ?").Scan(new(string)); err == nil {
		t.Fatal("missing parameter was accepted")
	}
	rows, err := db.Query("SELECT 1 AS value, 2 AS value UNION ALL SELECT 3, 4")
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	columns, err := rows.Columns()
	if err != nil || !reflect.DeepEqual(columns, []string{"value", "value"}) {
		t.Fatalf("columns = %v, %v", columns, err)
	}
	var values [][2]int
	for rows.Next() {
		var row [2]int
		if err := rows.Scan(&row[0], &row[1]); err != nil {
			t.Fatal(err)
		}
		values = append(values, row)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(values, [][2]int{{1, 2}, {3, 4}}) {
		t.Fatalf("rows = %v", values)
	}
	if err := db.QueryRow("SELECT 1 WHERE 0").Scan(new(int)); !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("empty row error = %v", err)
	}
}
