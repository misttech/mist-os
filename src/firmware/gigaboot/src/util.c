// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <limits.h>
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

// Wait for a keypress from a set of valid keys. If 0 < timeout_s < INT_MAX, the
// first key in the set of valid keys will be returned after timeout_s seconds
// if no other valid key is pressed.
char key_prompt(const char* valid_keys, int timeout_s) {
  if (strlen(valid_keys) < 1)
    return 0;
  if (timeout_s <= 0)
    return valid_keys[0];

  efi_status status;
  efi_event timer_event = NULL;
  if (timeout_s < INT_MAX) {
    status = gBS->CreateEvent(EVT_TIMER, 0, NULL, NULL, &timer_event);
    if (status != EFI_SUCCESS) {
      ELOG_S(status, "could not create event timer");
      return 0;
    }

    status = gBS->SetTimer(timer_event, TimerPeriodic, 10000000);
    if (status != EFI_SUCCESS) {
      ELOG_S(status, "could not set timer");
      return 0;
    }
  }

  bool cur_vis = gConOut->Mode->CursorVisible;
  int32_t col = gConOut->Mode->CursorColumn;
  int32_t row = gConOut->Mode->CursorRow;
  gConOut->EnableCursor(gConOut, false);

  char pressed = 0;
  if (timeout_s < INT_MAX) {
    LOG("Auto-boot in %ds", timeout_s);
  }
  do {
    int key;
    if (timeout_s == INT_MAX) {
      key = xefi_getc(-1);
    } else {
      key = xefi_getc(0);
    }

    if (key > 0) {
      char* which_key = strchr(valid_keys, key);
      if (which_key) {
        pressed = *which_key;
        break;
      }
    }

    if (timer_event != NULL && gBS->CheckEvent(timer_event) == EFI_SUCCESS) {
      timeout_s--;
      gConOut->SetCursorPosition(gConOut, col, row);
      LOG("Auto-boot in %ds", timeout_s);
    }
  } while (timeout_s);

  if (timer_event != NULL) {
    gBS->CloseEvent(timer_event);
  }
  gConOut->EnableCursor(gConOut, cur_vis);
  if (timeout_s > 0 && pressed) {
    return pressed;
  }

  // Default to first key in list
  return valid_keys[0];
}
