// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/channel.h>
#include <stdint.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include "deps.h"
#include "entry_point.h"

// This fake driver host waits for a driver start function address to be received on its
// bootstrap channel.
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
  // TODO(https://fxbug.dev/341998660): read from bootstrap channel to start the driver.

  return 0;
}
