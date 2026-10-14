// Copyright (c) 2024 Anton Zhiyanov, MIT License
// https://github.com/nalgeon/sqlean

// Case conversion functions for utf8 strings.

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#include "text/utf8/rune.h"
#include "text/utf8/utf8.h"

// utf8_put encodes the codepoint c at offset *out in dst, which has dstcap bytes,
// advancing *out by the number of bytes written. Always leaves room for a
// terminating zero. Returns false if c is not encodable or does not fit.
static bool utf8_put(char* dst, size_t dstcap, size_t* out, uint32_t c) {
    char buf[4];
    int len = utf8_encode(buf, c);
    if (len == 0 || *out + (size_t)len + 1 > dstcap) {
        return false;
    }
    memcpy(dst + *out, buf, (size_t)len);
    *out += (size_t)len;
    return true;
}

// utf8_transform converts the utf8 string src of n bytes using the transform
// function, writing the result to dst and its byte length to dstlen.
// ctx carries state between characters for transforms that need it.
//
// Returns false if src is not valid utf8 or the result does not fit in dst.
static bool utf8_transform(const char* src,
                           size_t n,
                           char* dst,
                           size_t dstcap,
                           size_t* dstlen,
                           uint32_t (*transform)(uint32_t, void*),
                           void* ctx) {
    if (dstcap == 0) {
        return false;
    }
    utf8_decode_t d = {.state = 0};
    size_t in = 0, out = 0;
    // Iterate over all n bytes: sqlite text may contain embedded zeros, and
    // U+0000 is a perfectly good codepoint to convert (to itself).
    while (in < n) {
        // Decode one character, advancing the input by the bytes it consumes
        // and never reading past the end of src.
        do {
            utf8_decode(&d, (uint8_t)src[in++]);
        } while (d.state && (in < n));
        if (d.state != 0) {
            // Invalid or truncated utf8 sequence.
            return false;
        }
        if (!utf8_put(dst, dstcap, &out, transform(d.codep, ctx))) {
            return false;
        }
    }
    dst[out] = '\0';
    *dstlen = out;
    return true;
}

static uint32_t transform_tolower(uint32_t c, void* ctx) {
    (void)ctx;
    return rune_tolower(c);
}

static uint32_t transform_toupper(uint32_t c, void* ctx) {
    (void)ctx;
    return rune_toupper(c);
}

static uint32_t transform_casefold(uint32_t c, void* ctx) {
    (void)ctx;
    return rune_casefold(c);
}

// transform_totitle uppercases the first character of each word and
// lowercases the rest. ctx points to the "at start of a word" flag.
static uint32_t transform_totitle(uint32_t c, void* ctx) {
    bool* upper = ctx;
    uint32_t res = *upper ? rune_toupper(c) : rune_tolower(c);
    *upper = !rune_isword(c);
    return res;
}

// utf8_tolower converts the utf8 string src to lowercase, writing the result to dst.
// Returns true if successful, false if an error occurred.
bool utf8_tolower(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen) {
    return utf8_transform(src, n, dst, dstcap, dstlen, transform_tolower, NULL);
}

// utf8_toupper converts the utf8 string src to uppercase, writing the result to dst.
bool utf8_toupper(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen) {
    return utf8_transform(src, n, dst, dstcap, dstlen, transform_toupper, NULL);
}

// utf8_casefold converts the utf8 string src to folded-case, writing the result to dst.
bool utf8_casefold(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen) {
    return utf8_transform(src, n, dst, dstcap, dstlen, transform_casefold, NULL);
}

// utf8_totitle converts the utf8 string src to title-case, writing the result to dst.
bool utf8_totitle(const char* src, size_t n, char* dst, size_t dstcap, size_t* dstlen) {
    bool upper = true;
    return utf8_transform(src, n, dst, dstcap, dstlen, transform_totitle, &upper);
}
