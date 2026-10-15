package boxdb

import (
	"context"
	"database/sql"
	"errors"
	"io/fs"
	"path"
	"unicode/utf8"
)

// MigrationFile is a named SQL document in the CLI's VERSION_NAME.sql format.
type MigrationFile struct {
	Filename string `json:"filename"`
	SQL      string `json:"sql"`
}

// MigrationInfo identifies a migration version.
type MigrationInfo struct {
	Version string `json:"version"`
	Name    string `json:"name"`
}

// MigrationState is the committed state of a migration.
type MigrationState string

const (
	MigrationApplied MigrationState = "applied"
	MigrationPending MigrationState = "pending"
)

// MigrationStatus describes a migration and its committed state.
type MigrationStatus struct {
	MigrationInfo
	State MigrationState `json:"state"`
}

// MigrationSet is an immutable, validated set of SQL documents.
// Its zero value is an empty set. It can be shared across goroutines.
type MigrationSet struct{ files []MigrationFile }

// NewMigrationSet validates and copies SQL documents without opening a database.
func NewMigrationSet(files []MigrationFile) (*MigrationSet, error) {
	documents := append([]MigrationFile{}, files...)
	for _, file := range documents {
		if !utf8.ValidString(file.Filename) || !utf8.ValidString(file.SQL) {
			return nil, validationError("migration documents must be valid UTF-8")
		}
	}
	if err := validateMigrations(documents); err != nil {
		return nil, err
	}
	return &MigrationSet{files: documents}, nil
}

// ReadMigrations reads a directory from an fs.FS, including os.DirFS and embed.FS.
func ReadMigrations(filesystem fs.FS, directory string) (*MigrationSet, error) {
	if filesystem == nil {
		return nil, validationError("migration filesystem must not be nil")
	}
	entries, err := fs.ReadDir(filesystem, directory)
	if err != nil {
		return nil, migrationIOError(err)
	}
	files := []MigrationFile{}
	for _, entry := range entries {
		if path.Ext(entry.Name()) != ".sql" {
			continue
		}
		sql, err := fs.ReadFile(filesystem, path.Join(directory, entry.Name()))
		if err != nil {
			return nil, migrationIOError(err)
		}
		files = append(files, MigrationFile{Filename: entry.Name(), SQL: string(sql)})
	}
	return NewMigrationSet(files)
}

func migrationIOError(err error) error {
	return &Error{Code: "BOXDB_IO", Message: err.Error(), cause: err}
}

// Up applies pending versions, committing each version independently.
func (m *MigrationSet) Up(ctx context.Context, db *sql.DB) ([]MigrationInfo, error) {
	var applied []MigrationInfo
	err := m.run(ctx, db, "migrations_up", &applied)
	return applied, err
}

// Down rolls back one version. An empty history returns nil, nil.
func (m *MigrationSet) Down(ctx context.Context, db *sql.DB) (*MigrationInfo, error) {
	var migration *MigrationInfo
	err := m.run(ctx, db, "migrations_down", &migration)
	return migration, err
}

// Status inspects a consistent snapshot without changing migration history.
func (m *MigrationSet) Status(ctx context.Context, db *sql.DB) ([]MigrationStatus, error) {
	var statuses []MigrationStatus
	err := m.run(ctx, db, "migrations_status", &statuses)
	return statuses, err
}

func (m *MigrationSet) run(ctx context.Context, db *sql.DB, operation string, out any) (err error) {
	if m == nil || db == nil {
		return validationError("migrations require a set and a BoxDB database")
	}
	connection, err := db.Conn(ctx)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, connection.Close()) }()
	return connection.Raw(func(raw any) error {
		c, ok := raw.(*conn)
		if !ok {
			return validationError("migrations require the BoxDB driver")
		}
		err := c.native.callContext(ctx, command{Operation: operation, Migrations: m.files}, out)
		if err != nil {
			return errors.Join(err, c.reset(context.Background()))
		}
		return nil
	})
}
