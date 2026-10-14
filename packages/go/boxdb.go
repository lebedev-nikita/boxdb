// Package boxdb provides a database/sql driver backed by the BoxDB Rust core.
package boxdb

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"strings"
	"time"
)

// Error is a failure reported by the BoxDB core.
type Error struct {
	Message      string `json:"message"`
	Code         string `json:"code"`
	ResultCode   *int   `json:"result_code"`
	ExtendedCode *int   `json:"extended_code"`
	cause        error
}

func (e *Error) Error() string { return "boxdb: " + e.Message }
func (e *Error) Unwrap() error { return e.cause }

func validationError(message string, args ...any) error {
	return &Error{Code: "BOXDB_VALIDATION", Message: fmt.Sprintf(message, args...)}
}

// Format controls the storage format of a new database file.
type Format string

const (
	Binary   Format = "binary"
	Readable Format = "readable"
)

type settings struct{ format Format }

// Option configures Open.
type Option func(*settings) error

// WithFormat selects the format for new files. Existing contents determine the format on reopen.
func WithFormat(format Format) Option {
	return func(s *settings) error {
		if format != Binary && format != Readable {
			return validationError("unsupported format %q", format)
		}
		s.format = format
		return nil
	}
}

// Open opens a database, or an isolated in-memory database for :memory:.
// It returns a database/sql pool limited to one connection.
func Open(path string, options ...Option) (*sql.DB, error) {
	configuration := settings{format: Binary}
	for _, option := range options {
		if option == nil {
			return nil, validationError("nil option")
		}
		if err := option(&configuration); err != nil {
			return nil, err
		}
	}
	db := sql.OpenDB(connector{path: path, format: configuration.format})
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if err := db.Ping(); err != nil {
		db.Close()
		return nil, err
	}
	return db, nil
}

func init() { sql.Register("boxdb", sqlDriver{}) }

type sqlDriver struct{}

func (sqlDriver) Open(path string) (driver.Conn, error) {
	return connector{path: path, format: Binary}.Connect(context.Background())
}

type connector struct {
	path   string
	format Format
}

func (connector) Driver() driver.Driver { return sqlDriver{} }
func (c connector) Connect(ctx context.Context) (driver.Conn, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	path := c.path
	if strings.ContainsRune(path, 0) {
		return nil, validationError("path contains NUL")
	}
	if path == "" {
		path = ":memory:"
	}
	native, err := openNative(path, c.format == Readable)
	if err != nil {
		return nil, err
	}
	return &conn{native: native}, nil
}

type conn struct {
	native  *nativeDB
	invalid bool
}

func (c *conn) Close() error {
	if c.native != nil {
		c.native.close()
		c.native = nil
	}
	return nil
}
func (c *conn) Prepare(query string) (driver.Stmt, error) {
	return c.PrepareContext(context.Background(), query)
}
func (c *conn) PrepareContext(ctx context.Context, query string) (driver.Stmt, error) {
	info, err := c.prepare(ctx, query)
	if err != nil {
		return nil, err
	}
	return &stmt{conn: c, query: query, parameters: info.Parameters}, nil
}
func (c *conn) Begin() (driver.Tx, error) {
	return c.BeginTx(context.Background(), driver.TxOptions{})
}
func (c *conn) BeginTx(ctx context.Context, options driver.TxOptions) (driver.Tx, error) {
	if options.Isolation != driver.IsolationLevel(sql.LevelDefault) && options.Isolation != driver.IsolationLevel(sql.LevelSerializable) {
		return nil, validationError("only default and serializable isolation are supported")
	}
	if options.ReadOnly {
		if _, err := c.run(ctx, "PRAGMA query_only=ON", nil); err != nil {
			return nil, err
		}
	}
	if _, err := c.run(ctx, "BEGIN", nil); err != nil {
		return nil, errors.Join(err, c.reset(context.Background()))
	}
	return &transaction{conn: c}, nil
}

func (c *conn) ResetSession(ctx context.Context) error {
	if err := c.reset(ctx); err != nil {
		if !c.IsValid() {
			return errors.Join(driver.ErrBadConn, err)
		}
		return err
	}
	return nil
}

func (c *conn) reset(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if !c.IsValid() {
		return &Error{Code: "BOXDB_UNAVAILABLE", Message: "connection is unavailable"}
	}
	var result struct{}
	if err := c.native.callContext(ctx, command{Operation: "reset"}, &result); err != nil {
		c.invalid = true
		return err
	}
	return nil
}
func (c *conn) IsValid() bool { return c.native != nil && !c.invalid }

type transaction struct{ conn *conn }

func (tx *transaction) finish(query string) error {
	_, err := tx.conn.run(context.Background(), query, nil)
	cleanup := tx.conn.reset(context.Background())
	return errors.Join(err, cleanup)
}
func (tx *transaction) Commit() error { return tx.finish("COMMIT") }
func (tx *transaction) Rollback() error {
	// SQLite may already have rolled back an interrupted transaction.
	return tx.conn.reset(context.Background())
}

type queryResult struct {
	Columns  []string            `json:"columns"`
	Rows     [][]json.RawMessage `json:"rows"`
	Affected int64               `json:"affected"`
	InsertID int64               `json:"last_insert_rowid"`
}

type command struct {
	Operation  string `json:"operation,omitempty"`
	SQL        string `json:"sql"`
	Parameters []any  `json:"parameters,omitempty"`
}
type statementInfo struct {
	Parameters []*string `json:"parameters"`
}

func (c *conn) prepare(ctx context.Context, query string) (*statementInfo, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if c.native == nil {
		return nil, driver.ErrBadConn
	}
	var info statementInfo
	err := c.native.callContext(ctx, command{Operation: "prepare", SQL: query}, &info)
	return &info, err
}

func (r *queryResult) LastInsertId() (int64, error) { return r.InsertID, nil }
func (r *queryResult) RowsAffected() (int64, error) { return r.Affected, nil }

func input(value driver.Value) (any, error) {
	switch value := value.(type) {
	case nil:
		return "Null", nil
	case int64:
		return map[string]any{"Integer": value}, nil
	case float64:
		if math.IsNaN(value) || math.IsInf(value, 0) {
			return nil, validationError("SQL numbers must be finite")
		}
		return map[string]any{"Real": value}, nil
	case bool:
		return map[string]any{"Boolean": value}, nil
	case string:
		return map[string]any{"Text": value}, nil
	case time.Time:
		return map[string]any{"Instant": value.UTC().Format(time.RFC3339Nano)}, nil
	case []byte:
		if value == nil {
			return "Null", nil
		}
		bytes := make([]int, len(value))
		for i, b := range value {
			bytes[i] = int(b)
		}
		return map[string]any{"Bytea": bytes}, nil
	default:
		return nil, validationError("unsupported parameter type %T", value)
	}
}

func (c *conn) execute(ctx context.Context, query string, args []driver.NamedValue) (*queryResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if c.native == nil {
		return nil, driver.ErrBadConn
	}
	for _, arg := range args {
		if arg.Name != "" {
			info, err := c.prepare(ctx, query)
			if err != nil {
				return nil, err
			}
			return c.executeBound(ctx, query, args, info.Parameters)
		}
	}
	parameters := make([]any, len(args))
	for i, arg := range args {
		value, err := input(arg.Value)
		if err != nil {
			return nil, err
		}
		parameters[i] = value
	}
	return c.run(ctx, query, parameters)
}

func (c *conn) run(ctx context.Context, query string, parameters []any) (*queryResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if c.native == nil {
		return nil, driver.ErrBadConn
	}
	if parameters == nil {
		parameters = []any{}
	}
	var result queryResult
	err := c.native.callContext(ctx, command{SQL: query, Parameters: parameters}, &result)
	return &result, err
}

func (c *conn) executeBound(ctx context.Context, query string, args []driver.NamedValue, names []*string) (*queryResult, error) {
	parameters := make([]any, len(names))
	bound := make([]bool, len(names))
	for _, arg := range args {
		value, err := input(arg.Value)
		if err != nil {
			return nil, err
		}
		matched := false
		for i, name := range names {
			match := arg.Name == "" && i == arg.Ordinal-1
			if arg.Name != "" {
				match = name != nil && strings.TrimLeft(*name, ":@$?") == arg.Name
			}
			if match {
				if bound[i] {
					return nil, validationError("parameter %d is bound twice", i+1)
				}
				parameters[i], bound[i], matched = value, true, true
			}
		}
		if !matched {
			return nil, validationError("unknown parameter %q at position %d", arg.Name, arg.Ordinal)
		}
	}
	for i, ok := range bound {
		if !ok {
			return nil, validationError("missing parameter %d", i+1)
		}
	}
	return c.run(ctx, query, parameters)
}

type stmt struct {
	conn       *conn
	query      string
	parameters []*string
	closed     bool
}

func (s *stmt) Close() error  { s.closed = true; return nil }
func (s *stmt) NumInput() int { return -1 }
func namedValues(values []driver.Value) []driver.NamedValue {
	args := make([]driver.NamedValue, len(values))
	for i, value := range values {
		args[i] = driver.NamedValue{Ordinal: i + 1, Value: value}
	}
	return args
}
func (s *stmt) Exec(values []driver.Value) (driver.Result, error) {
	return s.ExecContext(context.Background(), namedValues(values))
}
func (s *stmt) Query(values []driver.Value) (driver.Rows, error) {
	return s.QueryContext(context.Background(), namedValues(values))
}
func (s *stmt) ExecContext(ctx context.Context, args []driver.NamedValue) (driver.Result, error) {
	if s.closed {
		return nil, &Error{Code: "BOXDB_CLOSED", Message: "statement is closed"}
	}
	return s.conn.executeBound(ctx, s.query, args, s.parameters)
}
func (s *stmt) QueryContext(ctx context.Context, args []driver.NamedValue) (driver.Rows, error) {
	if s.closed {
		return nil, &Error{Code: "BOXDB_CLOSED", Message: "statement is closed"}
	}
	result, err := s.conn.executeBound(ctx, s.query, args, s.parameters)
	if err != nil {
		return nil, err
	}
	return &rows{result: result}, nil
}
func (c *conn) ExecContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Result, error) {
	if len(args) == 0 {
		if !c.IsValid() {
			return nil, driver.ErrBadConn
		}
		var result queryResult
		err := c.native.callContext(ctx, command{Operation: "exec", SQL: query}, &result)
		return &result, err
	}
	return c.execute(ctx, query, args)
}
func (c *conn) QueryContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Rows, error) {
	result, err := c.execute(ctx, query, args)
	if err != nil {
		return nil, err
	}
	return &rows{result: result}, nil
}

type rows struct {
	result *queryResult
	index  int
}

func (r *rows) Columns() []string { return r.result.Columns }
func (r *rows) Close() error      { r.result.Rows = nil; return nil }
func (r *rows) Next(dest []driver.Value) error {
	if r.index >= len(r.result.Rows) {
		return io.EOF
	}
	for i, raw := range r.result.Rows[r.index] {
		if string(raw) == `"Null"` {
			dest[i] = nil
			continue
		}
		var value map[string]json.RawMessage
		if err := json.Unmarshal(raw, &value); err != nil {
			return err
		}
		for kind, data := range value {
			switch kind {
			case "Integer":
				var v int64
				if err := json.Unmarshal(data, &v); err != nil {
					return err
				}
				dest[i] = v
			case "Real":
				var v float64
				if err := json.Unmarshal(data, &v); err != nil {
					return err
				}
				dest[i] = v
			case "Text":
				var v string
				if err := json.Unmarshal(data, &v); err != nil {
					return err
				}
				dest[i] = v
			case "Bytea":
				var v []byte
				if err := json.Unmarshal(data, &v); err != nil {
					return err
				}
				dest[i] = v
			default:
				return &Error{Code: "BOXDB_INTERNAL", Message: fmt.Sprintf("unexpected storage type %s", kind)}
			}
		}
	}
	r.index++
	return nil
}
