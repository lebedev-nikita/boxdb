// Copyright (c) 2023 Anton Zhiyanov, MIT License
// https://github.com/nalgeon/sqlean

// UTF-8 characters (runes) <-> C string conversions.

#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#include "text/runes.h"
#include "text/utf8/utf8.h"

// runes_from_cstring decodes n bytes into runes and stores their count.
int32_t* runes_from_cstring(const char* const str, size_t n, size_t* count) {
    assert(n > 0);
    // one rune per byte is the worst case (ascii, or U+FFFD substitutions)
    int32_t* runes = malloc(n * sizeof(int32_t));
    if (runes == NULL) {
        *count = 0;
        return NULL;
    }

    size_t i = 0, idx = 0;
    while (i < n) {
        runes[idx++] = (int32_t)utf8_next(str, n, &i);
    }
    *count = idx;

    if (idx < n) {
        // shrink to real size
        int32_t* shrunk = realloc(runes, idx * sizeof(int32_t));
        if (shrunk != NULL) {
            runes = shrunk;
        }
    }
    return runes;
}

// runes_to_cstring creates a C string from an array of runes.
char* runes_to_cstring(const int32_t* runes, size_t length) {
    char* str;
    if (length == 0) {
        str = calloc(1, sizeof(char));
        return str;
    }

    size_t maxlen = length * sizeof(int32_t) + 1;
    str = malloc(maxlen);
    if (str == NULL) {
        return NULL;
    }

    char* at = str;
    for (size_t i = 0; i < length; i++) {
        at += utf8_encode(at, runes[i]);
    }
    *at = '\0';
    at += 1;

    if ((size_t)(at - str) < maxlen) {
        // shrink to real size
        size_t size = at - str;
        str = realloc(str, size);
    }
    return str;
}
