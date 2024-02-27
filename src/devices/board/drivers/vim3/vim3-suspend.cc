// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/types.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>

#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

fpbus::Node suspend_node = []() {
  fpbus::Node dev = {};
  dev.name() = "vim3-suspend";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_SUSPEND_HAL;
  return dev;
}();

zx_status_t Vim3::SuspendInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SUSP');

  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, suspend_node));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd FIDL call failed for vim3-suspend: %s",
           result.FormatDescription().data());
    return result.status();
  }

  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd failed for vim3-suspend: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
