// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/asm.h>

.arch_extension mops

.function memset, global

  // The return value is always just the first argument and they both go in x0.
  // So use a different register for the destination pointer that gets updated.
  retval .req x0
  dst .req x3
  byte .req x1
  count .req x2

  mov dst, retval

  setp [dst]!, count!, byte
  setm [dst]!, count!, byte
  sete [dst]!, count!, byte

  ret

.end_function
