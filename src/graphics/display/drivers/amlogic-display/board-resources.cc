// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"

#include <lib/ddk/debug.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cstdint>

namespace amlogic_display {

zx::result<BoardInfo> GetBoardInfo(ddk::PDevFidl& platform_device) {
  pdev_board_info_t banjo_board_info;
  zx_status_t status = platform_device.GetBoardInfo(&banjo_board_info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get board info: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  BoardInfo board_info = {
      .board_vendor_id = banjo_board_info.vid,
      .board_product_id = banjo_board_info.pid,
  };
  return zx::ok(board_info);
}

zx::result<fdf::MmioBuffer> MapMmio(MmioResourceIndex mmio_index, ddk::PDevFidl& platform_device) {
  std::optional<fdf::MmioBuffer> mmio;
  zx_status_t status = platform_device.MapMmio(static_cast<uint32_t>(mmio_index), &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map MMIO resource #%" PRIu32 ": %s", static_cast<uint32_t>(mmio_index),
           zx_status_get_string(status));
    return zx::error(status);
  }

  ZX_DEBUG_ASSERT_MSG(mmio.has_value(), "MapMmio() succeeded but didn't populate the out-param");
  return zx::ok(std::move(mmio).value());
}

zx::result<zx::interrupt> GetInterrupt(InterruptResourceIndex interrupt_index,
                                       ddk::PDevFidl& platform_device) {
  zx::interrupt interrupt;
  zx_status_t status =
      platform_device.GetInterrupt(static_cast<uint32_t>(interrupt_index), &interrupt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get Interrupt resource #%" PRIu32 ": %s",
           static_cast<uint32_t>(interrupt_index), zx_status_get_string(status));
    return zx::error(status);
  }
  ZX_DEBUG_ASSERT_MSG(interrupt.is_valid(),
                      "GetInterrupt() succeeded but didn't populate the out-param");
  return zx::ok(std::move(interrupt));
}

zx::result<zx::bti> GetBti(BtiResourceIndex bti_index, ddk::PDevFidl& platform_device) {
  zx::bti bti;
  zx_status_t status = platform_device.GetBti(static_cast<uint32_t>(bti_index), &bti);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI resource #%" PRIu32 ": %s", static_cast<uint32_t>(bti_index),
           zx_status_get_string(status));
    return zx::error(status);
  }
  ZX_DEBUG_ASSERT_MSG(bti.is_valid(), "GetBti() succeeded but didn't populate the out-param");
  return zx::ok(std::move(bti));
}

zx::result<zx::resource> GetSecureMonitorCall(
    SecureMonitorCallResourceIndex secure_monitor_call_index, ddk::PDevFidl& platform_device) {
  zx::resource secure_monitor_call;
  zx_status_t status = platform_device.GetSmc(static_cast<uint32_t>(secure_monitor_call_index),
                                              &secure_monitor_call);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get SMC resource #%" PRIu32 ": %s",
           static_cast<uint32_t>(secure_monitor_call_index), zx_status_get_string(status));
    return zx::error(status);
  }
  ZX_DEBUG_ASSERT_MSG(secure_monitor_call.is_valid(),
                      "GetSmc() succeeded but didn't populate the out-param");
  return zx::ok(std::move(secure_monitor_call));
}

}  // namespace amlogic_display
