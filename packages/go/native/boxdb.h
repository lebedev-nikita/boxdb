#ifndef BOXDB_GO_H
#define BOXDB_GO_H

#include <stdbool.h>

typedef struct Handle boxdb_go_handle;
boxdb_go_handle *boxdb_go_open(const char *path, bool readable, char **error);
char *boxdb_go_call(const boxdb_go_handle *handle, const char *request);
void boxdb_go_interrupt(const boxdb_go_handle *handle);
void boxdb_go_clear_interrupt(const boxdb_go_handle *handle);
void boxdb_go_close(boxdb_go_handle *handle);
void boxdb_go_free(char *value);

#endif
