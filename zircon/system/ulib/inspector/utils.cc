// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <elf.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>

#include "inspector/inspector.h"
#include "utils-impl.h"

namespace inspector {

// Same as basename, except will not modify |path|.
// Returns "" if |path| has a trailing /.

const char* path_basename(const char* path) {
  const char* base = strrchr(path, '/');
  if (base == nullptr)
    return path;
  return base + 1;
}

void do_print_error(const char* file, int line, const char* fmt, ...) {
  const char* base = path_basename(file);
  va_list args;
  va_start(args, fmt);
  fprintf(stderr, "inspector: %s:%d: ", base, line);
  vfprintf(stderr, fmt, args);
  fprintf(stderr, "\n");
  va_end(args);
}

void do_print_zx_error(const char* file, int line, const char* what, zx_status_t status) {
  do_print_error(file, line, "%s: %d (%s)", what, status, zx_status_get_string(status));
}

}  // namespace inspector
