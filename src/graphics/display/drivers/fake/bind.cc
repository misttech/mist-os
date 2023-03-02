// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/platform-defs.h>

#include <fbl/alloc_checker.h>

#include "fake-display.h"
#include "src/graphics/display/drivers/fake/fake-display-bind.h"

namespace fake_display {

namespace {

// main bind function called from dev manager
zx_status_t CreateAndBindFakeDisplay(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto fake_display = fbl::make_unique_checked<fake_display::FakeDisplay>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = fake_display->Bind(/*start_vsync_thread=*/true);
  if (status == ZX_OK) {
    // devmgr now owns `fake_display`
    [[maybe_unused]] auto ptr = fake_display.release();
  }
  return status;
}

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = CreateAndBindFakeDisplay,
};

}  // namespace

}  // namespace fake_display

// clang-format off
ZIRCON_DRIVER(fake_display, fake_display::kDriverOps, "zircon", "0.1");
