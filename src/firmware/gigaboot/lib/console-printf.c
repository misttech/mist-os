// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <printf.h>
#include <xefi.h>

#define PCBUFMAX 126
// buffer is two larger to leave room for a \0 and room
// for a \r that may be added after a \n
typedef struct {
  int off;
  char16_t buf[PCBUFMAX + 2];
} _pcstate;

static int _printf_console_out(const char* str, size_t len, void* _state) {
  _pcstate* state = _state;
  char16_t* buf = state->buf;
  int i = state->off;
  int n = len;
  while (n > 0) {
    if (*str == '\n') {
      buf[i++] = '\r';
    }
    buf[i++] = *str++;
    if (i >= PCBUFMAX) {
      buf[i] = 0;
      if (write_to_serial(buf, i) < 0) {
        return -1;
      }
      gConOut->OutputString(gConOut, buf);
      i = 0;
    }
    n--;
  }
  state->off = i;
  return len;
}

int printf(const char* fmt, ...) {
  va_list ap;
  int r;
  va_start(ap, fmt);
  r = vprintf(fmt, ap);
  va_end(ap);
  return r;
}

int vprintf(const char* fmt, va_list ap) {
  _pcstate state;
  int r;
  state.off = 0;
  r = _printf_engine(_printf_console_out, &state, fmt, ap);
  if (state.off) {
    state.buf[state.off] = 0;
    write_to_serial(state.buf, state.off);
    gConOut->OutputString(gConOut, state.buf);
  }
  return r;
}
