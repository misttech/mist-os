// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/channel.h>
#include <stdint.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include "deps.h"
#include "driver_entry_point.h"
#include "entry_point.h"
#include "fake_runtime.h"

// This fake driver host waits for messages to be received on the bootstrap channel.
// Each message will contain the start function address of a loaded driver.
// If the driver host receives a null address, the driver host will exit and
// return the summation of the return values from calling each driver start function.
extern "C" int64_t Start(zx_handle_t bootstrap, void* vdso) {
  zx::channel channel{bootstrap};

  zx_signals_t signals;
  int64_t total = 0;
  while (true) {
    // Wait for the driver manager to send the address of the fake driver's DriverStart()
    // function.
    zx_status_t status = channel.wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), &signals);
    if (status != ZX_OK) {
      return status;
    }
    if (!(signals & ZX_CHANNEL_READABLE)) {
      return ZX_ERR_BAD_STATE;
    }

    uintptr_t ptr;
    uint32_t actual_bytes, actual_handles;
    status = channel.read(0, &ptr, nullptr, sizeof(ptr), 0, &actual_bytes, &actual_handles);
    if (status != ZX_OK) {
      return status;
    }
    if (actual_bytes != sizeof(ptr)) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    // Received a null address, signalling that the driver host should exit.
    if (ptr == 0) {
      return total;
    }
    const auto driver_start_func = reinterpret_cast<decltype(DriverStart)*>(ptr);
    total += driver_start_func();
  }
  return total;
}

// Implementation of fake_runtime.h.
__EXPORT int64_t runtime_get_id() { return a() + 1; }
