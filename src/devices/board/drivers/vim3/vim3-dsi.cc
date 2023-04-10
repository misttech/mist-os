// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

static const std::vector<fpbus::Mmio> kDsiMmios = {
    {{
        // DSI Host Controller
        .base = A311D_MIPI_DSI_BASE,
        .length = A311D_MIPI_DSI_LENGTH,
    }},
};

static const fpbus::Node kDsiDeviceNode = []() {
  fpbus::Node dev = {};
  dev.name() = "dw-dsi";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_DW_DSI;
  dev.mmio() = kDsiMmios;
  return dev;
}();
}  // namespace

zx_status_t Vim3::DsiInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DSI_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, kDsiDeviceNode));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Dsi(dw-dsi) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Dsi(dw-dsi) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace vim3
