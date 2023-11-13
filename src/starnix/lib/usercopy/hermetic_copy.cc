// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hermetic_copy.h"

typedef uintptr_t word;
constexpr size_t kWordSize = sizeof(word);
constexpr size_t kWordSizeMask = kWordSize - 1;

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
                                                        size_t count, bool ret_dest) {
  if ((reinterpret_cast<uintptr_t>(dest) | reinterpret_cast<uintptr_t>(source)) & kWordSizeMask) {
    size_t len;
    // src and/or dest do not align on word boundary
    if (((reinterpret_cast<uintptr_t>(dest) ^ reinterpret_cast<uintptr_t>(source)) &
         kWordSizeMask) ||
        (count < kWordSize)) {
      len = count;  // copy the rest of the buffer with the byte mover
    } else {
      len = kWordSize - (reinterpret_cast<uintptr_t>(dest) &
                         kWordSizeMask);  // move the ptrs up to a word boundary
    }
    count -= len;
    for (; len > 0; len--) {
      *dest++ = *source++;
    }
  }
  for (; count >= kWordSize; count -= kWordSize) {
    *reinterpret_cast<volatile word*>(dest) = *reinterpret_cast<const volatile word*>(source);
    dest += kWordSize;
    source += kWordSize;
  }
  for (; count > 0; count--) {
    *dest++ = *source++;
  }

  return ret_dest ? reinterpret_cast<uintptr_t>(dest) : reinterpret_cast<uintptr_t>(source);
}
