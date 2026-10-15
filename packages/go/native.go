package boxdb

/*
#cgo LDFLAGS: -L${SRCDIR}/native/lib -lboxdb_go
#cgo darwin LDFLAGS: -framework CoreFoundation -framework Security -liconv
#cgo linux LDFLAGS: -ldl -lpthread -lm
#include <stdlib.h>
#include "native/boxdb.h"
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"time"
	"unsafe"
)

func (db *nativeDB) callContext(ctx context.Context, command any, out any) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	C.boxdb_go_clear_interrupt(db.handle)
	if ctx.Done() == nil {
		return db.call(command, out)
	}
	done, stopped := make(chan struct{}), make(chan struct{})
	go func() {
		defer close(stopped)
		select {
		case <-done:
			return
		case <-ctx.Done():
		}
		// Repeat until execution finishes, covering cancellation before SQLite starts.
		ticker := time.NewTicker(time.Millisecond)
		defer ticker.Stop()
		for {
			C.boxdb_go_interrupt(db.handle)
			select {
			case <-done:
				return
			case <-ticker.C:
			}
		}
	}()
	err := db.call(command, out)
	close(done)
	<-stopped // No interrupt may race with the next operation or Close.
	if err != nil && ctx.Err() != nil {
		var failure *Error
		if errors.As(err, &failure) && (failure.Code == "BOXDB_INTERRUPT" || failure.Code == "BOXDB_BUSY" || failure.Code == "BOXDB_LOCKED") {
			return errors.Join(ctx.Err(), err)
		}
	}
	return err
}

type nativeDB struct{ handle *C.boxdb_go_handle }

func openNative(path string, readable bool) (*nativeDB, error) {
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	var failure *C.char
	handle := C.boxdb_go_open(cpath, C.bool(readable), &failure)
	if handle == nil {
		defer C.boxdb_go_free(failure)
		var err Error
		if decode := json.Unmarshal([]byte(C.GoString(failure)), &err); decode != nil {
			return nil, &Error{Code: "BOXDB_INTERNAL", Message: "invalid native open response", cause: decode}
		}
		return nil, &err
	}
	return &nativeDB{handle: handle}, nil
}

func (db *nativeDB) call(command any, out any) error {
	data, err := json.Marshal(command)
	if err != nil {
		return &Error{Code: "BOXDB_VALIDATION", Message: err.Error(), cause: err}
	}
	request := C.CString(string(data))
	defer C.free(unsafe.Pointer(request))
	answer := C.boxdb_go_call(db.handle, request)
	return decodeNative(answer, out)
}

func validateMigrations(files []MigrationFile) error {
	data, err := json.Marshal(files)
	if err != nil {
		return validationError("cannot encode migrations: %v", err)
	}
	request := C.CString(string(data))
	defer C.free(unsafe.Pointer(request))
	var result struct{}
	return decodeNative(C.boxdb_go_validate_migrations(request), &result)
}

func decodeNative(answer *C.char, out any) error {
	defer C.boxdb_go_free(answer)
	var envelope struct {
		Result json.RawMessage `json:"result"`
		Error  *Error          `json:"error"`
	}
	if err := json.Unmarshal([]byte(C.GoString(answer)), &envelope); err != nil {
		return &Error{Code: "BOXDB_INTERNAL", Message: "invalid native response", cause: err}
	}
	if envelope.Error != nil {
		return envelope.Error
	}
	if err := json.Unmarshal(envelope.Result, out); err != nil {
		return &Error{Code: "BOXDB_INTERNAL", Message: "invalid native result", cause: err}
	}
	return nil
}

func (db *nativeDB) close() { C.boxdb_go_close(db.handle) }
