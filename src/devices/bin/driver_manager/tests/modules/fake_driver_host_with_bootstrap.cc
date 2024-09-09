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

// This fake driver host waits for a driver start function address to be received on its
// bootstrap channel, then returns the result of calling the driver start function.
extern "C" int64_t Start(zx_handle_t bootstrap, void* vdso) {
  zx::channel channel{bootstrap};

  // Wait for the driver manager to send the address of the fake root driver's DriverStart()
  // function.
  zx_signals_t signals;
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
  // Check that we get the value returned by the dh-deps-a.cc implementation,
  // not fake_root_driver_deps.cc.
  // In this main executable's domain a() -> (b() -> 6) + (c() -> 7) -> 13.
  if (a() != 13) {
    return ZX_ERR_INTERNAL;
  }

  const auto driver_start_func = reinterpret_cast<decltype(DriverStart)*>(ptr);
  return driver_start_func();
}

// Implementation of fake_runtime.h.
__EXPORT int64_t runtime_get_id() { return a() + 1; }
