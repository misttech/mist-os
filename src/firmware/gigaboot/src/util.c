// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <log.h>
#include <stdio.h>
#include <string.h>
#include <uchar.h>
#include <xefi.h>

#include <efi/system-table.h>
#include <efi/types.h>

uint64_t htonll(uint64_t val) {
#if __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
  return __builtin_bswap64(val);
#else
  return val;
#endif
}

uint64_t ntohll(uint64_t val) { return htonll(val); }

bool uefi_realloc(void** buf, size_t old_size, size_t new_size) {
  char16_t* new_buffer = NULL;
  efi_status status;

  if (!buf)
    return false;

  // Free buffer if `new_size == 0`
  if (new_size == 0) {
    if (!*buf)
      return true;

    status = gSys->BootServices->FreePool(*buf);
    if (EFI_ERROR(status)) {
      ELOG_S(status, "Realloc: old buffer free failed");
      return false;
    }
    return true;
  }

  if (old_size == new_size)
    return true;

  status = gSys->BootServices->AllocatePool(EfiLoaderData, new_size, (void*)&new_buffer);
  if (EFI_ERROR(status)) {
    ELOG_S(status, "Realloc: new buffer allocation failed");
    return false;
  }

  if (*buf) {
    memcpy(new_buffer, *buf, MIN(old_size, new_size));

    status = gSys->BootServices->FreePool(*buf);
    if (EFI_ERROR(status)) {
      ELOG_S(status, "Realloc: old buffer free failed");
      return false;
    }
  }

  *buf = new_buffer;

  return true;
}
