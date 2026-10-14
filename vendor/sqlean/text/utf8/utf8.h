// Copyright (c) 2024 Anton Zhiyanov, MIT License
// https://github.com/nalgeon/sqlean

// UTF-8 string handling.

#ifndef UTF8_H
#define UTF8_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// decode next utf8 codepoint.
// See https://bjoern.hoehrmann.de/utf-8/decoder/dfa/ for details.
typedef struct {
    uint32_t state, codep;
} utf8_decode_t;

// utf8_decode decodes a byte as part of a utf8 codepoint.
uint32_t utf8_decode(utf8_decode_t* d, const uint32_t byte);
// utf8_encode encodes the utf8 codepoint c to s
// and returns the number of bytes written.
int utf8_encode(char* out, uint32_t c);

// utf8_next decodes the next codepoint and advances the byte index.
uint32_t utf8_next(const char* s, size_t n, size_t* i);

// utf8_icmp compares the utf8 strings s1 and s2 case-insensitively.
int utf8_icmp(const char* s1, size_t n1, const char* s2, size_t n2);

// utf8_tolower converts the utf8 string src to lowercase.
bool utf8_tolower(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen);
// utf8_toupper converts the utf8 string src to uppercase.
bool utf8_toupper(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen);
// utf8_totitle converts the utf8 string src to title-case.
bool utf8_totitle(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen);
// utf8_casefold converts the utf8 string src to folded-case.
bool utf8_casefold(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen);

#endif  // UTF8_H
