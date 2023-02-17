// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Small utility functions.

#ifndef SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_
#define SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

#define MIN(a, b) (((a) < (b)) ? (a) : (b))
#define MAX(a, b) (((a) > (b)) ? (a) : (b))

uint64_t htonll(uint64_t val);
uint64_t ntohll(uint64_t val);

// Simple implementation of ReallocatePool() from UEFI.
// Using this UEFI version would require memory leak mechanism update.
// Follows standard `void *realloc(void *ptr, size_t size);` logic:
//
// Function changes the size of the memory block pointed  to by `*buf` to `new_size` bytes.  The
// contents will be unchanged in the range from the start of the region up to the minimum of the old
// and new sizes.  If the  new size is larger than the old size, the added memory will not be
// initialized.  If `*buf` is `NULL`, then  the  call  is  equivalent  to
// `AllocatePool(_,buf,new_size)`, for all values of `new_size`; if `new_size` is equal to zero, and
// `*buf` is not `NULL`, then the call is equivalent to `FreePool(*buf)`. Unless `*buf` is `NULL`,
// it must have been returned by an earlier call to `AllocatePool()` or `efi_realloc()`. If the area
// pointed to was moved, a `FreePool(*buf)` is done. Note that if `buf == NULL` function will fail.
//
// Returns true on success and false on failure.
bool uefi_realloc(void** buf, size_t old_size, size_t new_size);

char key_prompt(const char* valid_keys, int timeout_s);

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_
