// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hermetic_copy.h"

// TODO(https://g-issues.fuchsia.dev/issues/309108366): Read more than 1 byte
// per loop iterator.
//
// The source and destination buffers are marked volatile so that the read/write
// of each bytes is considered a visible side-effect and prevents the compiler
// from attempting to read memory from multiple pages in an instruction (e.g.
// MOVUPS with XMM registers). We need to make sure each access happens within a
// page so that we read/write as much bytes as possible before faulting and
// ending the copy.
//
// See the change that introduced the volatile qualifier for the buffers for
// and https://g-issues.fuchsia.dev/issues/309108366 for more details.
[[gnu::section(".text.entry")]] uintptr_t hermetic_copy(volatile uint8_t* dest,
                                                        const volatile uint8_t* source,
                                                        size_t len) {
  for (size_t i = 0; i < len; ++i) {
    dest[i] = source[i];
  }
  return 0;
}
