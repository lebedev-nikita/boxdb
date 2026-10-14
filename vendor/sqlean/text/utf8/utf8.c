/* MIT License
 *
 * Copyright (c) 2023 Tyge Løvset
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

// UTF-8 string handling.

#include <stdint.h>

#include "text/utf8/rune.h"
#include "text/utf8/utf8.h"

const uint8_t utf8_dtab[] = {
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,
    1,  1,  1,  1,  1,  1,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  7,
    7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,  7,
    7,  7,  7,  7,  7,  7,  7,  7,  8,  8,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,
    2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  2,  10, 3,  3,  3,  3,  3,
    3,  3,  3,  3,  3,  3,  3,  4,  3,  3,  11, 6,  6,  6,  5,  8,  8,  8,  8,  8,  8,  8,  8,
    8,  8,  8,  0,  12, 24, 36, 60, 96, 84, 12, 12, 12, 48, 72, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 0,  12, 12, 12, 12, 12, 0,  12, 0,  12, 12, 12, 24, 12, 12, 12, 12, 12,
    24, 12, 24, 12, 12, 12, 12, 12, 12, 12, 12, 12, 24, 12, 12, 12, 12, 12, 24, 12, 12, 12, 12,
    12, 12, 12, 24, 12, 12, 12, 12, 12, 12, 12, 12, 12, 36, 12, 36, 12, 12, 12, 36, 12, 12, 12,
    12, 12, 36, 12, 36, 12, 12, 12, 36, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
};

// Encode/decode functions.

// utf8_decode decodes a byte as part of a utf8 codepoint.
uint32_t utf8_decode(utf8_decode_t* d, const uint32_t byte) {
    const uint32_t type = utf8_dtab[byte];
    d->codep = d->state ? (byte & 0x3fu) | (d->codep << 6) : (0xffU >> type) & byte;
    return d->state = utf8_dtab[256 + d->state + type];
}

// utf8_encode encodes the utf8 codepoint c to s
// and returns the number of bytes written.
int utf8_encode(char* out, uint32_t c) {
    if (c < 0x80U) {
        out[0] = (char)c;
        return 1;
    } else if (c < 0x0800U) {
        out[0] = (char)((c >> 6 & 0x1F) | 0xC0);
        out[1] = (char)((c & 0x3F) | 0x80);
        return 2;
    } else if (c < 0x010000U) {
        if ((c < 0xD800U) | (c >= 0xE000U)) {
            out[0] = (char)((c >> 12 & 0x0F) | 0xE0);
            out[1] = (char)((c >> 6 & 0x3F) | 0x80);
            out[2] = (char)((c & 0x3F) | 0x80);
            return 3;
        }
    } else if (c < 0x110000U) {
        out[0] = (char)((c >> 18 & 0x07) | 0xF0);
        out[1] = (char)((c >> 12 & 0x3F) | 0x80);
        out[2] = (char)((c >> 6 & 0x3F) | 0x80);
        out[3] = (char)((c & 0x3F) | 0x80);
        return 4;
    }
    return 0;
}

// String functions.

// UTF8_REJECT is the decoder state after a byte that cannot appear in a valid
// utf8 sequence at that position. The reject state is sticky: feeding the
// decoder more bytes never gets it out, so a loop waiting for the accept
// state must treat it as final.
#define UTF8_REJECT 12

// utf8_next decodes the codepoint starting at byte *i of s, which has n bytes,
// and advances *i past it. It never reads past the n bytes, and *i always
// advances, so a loop over utf8_next visits every byte exactly once.
// An invalid or truncated sequence decodes as U+FFFD, the replacement
// character, one per maximal ill-formed subpart: the bytes that form a valid
// prefix collapse into a single U+FFFD, and the byte that broke the sequence
// is left for the next call, since it may well start a valid one.
uint32_t utf8_next(const char* s, size_t n, size_t* i) {
    if (*i >= n) {
        return 0;
    }
    const size_t start = *i;
    utf8_decode_t d = {.state = 0};
    uint32_t state;
    do {
        state = utf8_decode(&d, (uint8_t)s[(*i)++]);
    } while (state != 0 && state != UTF8_REJECT && *i < n);
    if (state == 0) {
        return d.codep;
    }
    if (state == UTF8_REJECT && *i - start > 1) {
        // the last byte broke a sequence it was never part of:
        // give it back so the next call can try it as a sequence start
        (*i)--;
    }
    return 0xFFFD;
}

// utf8_icmp compares the utf8 strings s1 and s2 case-insensitively.
int utf8_icmp(const char* s1, size_t n1, const char* s2, size_t n2) {
    size_t j1 = 0, j2 = 0;
    while ((j1 < n1) & (j2 < n2)) {
        uint32_t c1 = utf8_next(s1, n1, &j1);
        uint32_t c2 = utf8_next(s2, n2, &j2);
        int32_t c = (int32_t)rune_casefold(c1) - (int32_t)rune_casefold(c2);
        if (c || !s2[j2 - 1])  // OK if n1 and n2 are npos
            return (int)c;
    }
    // the string whose codepoints ran out first is the smaller one.
    // byte lengths cannot decide this: a casefold may change the encoded
    // width (ſ is two bytes but folds to the one-byte s), so equal decoded
    // streams can have unequal byte lengths
    return (j1 < n1) - (j2 < n2);
}
