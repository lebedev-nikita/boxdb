package boxdb

import (
	"database/sql/driver"
	"encoding/json"
	"io"
	"strings"
)

// Decimal is exact decimal text. Use a DECIMAL column or explicit SQL CAST;
// BoxDB validates precision and scale without a floating-point conversion.
type Decimal string

func (value Decimal) Value() (driver.Value, error) { return string(value), nil }
func (value *Decimal) Scan(source any) error       { return scanNative(value, source) }

// UUID is canonical UUID text when read from a UUID column.
type UUID string

// Instant is UTC RFC 3339 text with microsecond precision when read from INSTANT.
type Instant string

// Date is a calendar date without a time zone.
type Date string

// Time is a time of day without a date or time zone, with microsecond precision.
type Time string

// DateTime is a calendar date and time without a time zone.
type DateTime string

// Interval preserves calendar months, days and microseconds separately.
type Interval string

func (value UUID) Value() (driver.Value, error)     { return string(value), nil }
func (value Instant) Value() (driver.Value, error)  { return string(value), nil }
func (value Date) Value() (driver.Value, error)     { return string(value), nil }
func (value Time) Value() (driver.Value, error)     { return string(value), nil }
func (value DateTime) Value() (driver.Value, error) { return string(value), nil }
func (value Interval) Value() (driver.Value, error) { return string(value), nil }
func (value *UUID) Scan(source any) error           { return scanNative(value, source) }
func (value *Instant) Scan(source any) error        { return scanNative(value, source) }
func (value *Date) Scan(source any) error           { return scanNative(value, source) }
func (value *Time) Scan(source any) error           { return scanNative(value, source) }
func (value *DateTime) Scan(source any) error       { return scanNative(value, source) }
func (value *Interval) Scan(source any) error       { return scanNative(value, source) }

func scanNative[T ~string](target *T, source any) error {
	text, err := scanText(source)
	if err != nil {
		return err
	}
	*target = T(text)
	return nil
}

func scanText(source any) (string, error) {
	switch source := source.(type) {
	case string:
		return source, nil
	case []byte:
		return string(source), nil
	case nil:
		return "", validationError("cannot scan SQL NULL into a native value; use sql.Null[T]")
	default:
		return "", validationError("cannot scan %T into native text", source)
	}
}

// Changeset holds recorded row changes in SQLite's changeset format, as read
// from a CHANGESET column or returned by boxdb_changeset(). Apply it with
// SELECT boxdb_apply(?, ?) inside a transaction.
type Changeset []byte

func (value Changeset) Value() (driver.Value, error) { return []byte(value), nil }

func (value *Changeset) Scan(source any) error {
	switch source := source.(type) {
	case []byte:
		*value = append(Changeset(nil), source...)
		return nil
	case nil:
		return validationError("cannot scan SQL NULL into a changeset; use sql.Null[Changeset]")
	default:
		return validationError("cannot scan %T into a changeset", source)
	}
}

// JSON encodes and decodes a complete document in the client. Use sql.Null[JSON[T]]
// for SQL NULL; a nil Data value represents the JSON document null.
// Decoding into interface values preserves numbers as json.Number.
type JSON[T any] struct{ Data T }

func (value JSON[T]) Value() (driver.Value, error) {
	bytes, err := json.Marshal(value.Data)
	if err != nil {
		return nil, err
	}
	return string(bytes), nil
}

func (value *JSON[T]) Scan(source any) error {
	text, err := scanText(source)
	if err != nil {
		return err
	}
	decoder := json.NewDecoder(strings.NewReader(text))
	decoder.UseNumber()
	var data T
	if err := decoder.Decode(&data); err != nil {
		return err
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		if err != nil {
			return err
		}
		return validationError("expected one JSON document")
	}
	value.Data = data
	return nil
}

func (value JSON[T]) MarshalJSON() ([]byte, error) { return json.Marshal(value.Data) }

func (value *JSON[T]) UnmarshalJSON(bytes []byte) error { return value.Scan(bytes) }
