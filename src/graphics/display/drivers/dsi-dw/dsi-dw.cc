// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/dsi-dw/dsi-dw.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <optional>

#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/designware-dsi/dsi-host-controller.h"

namespace dsi_dw {

DsiDw::DsiDw(zx_device_t* parent, fdf::MmioBuffer dsi_mmio)
    : DeviceType(parent), dsi_host_controller_(std::move(dsi_mmio)) {}

DsiDw::~DsiDw() = default;

void DsiDw::DdkRelease() { delete this; }

zx_status_t DsiDw::DsiImplConfig(const dsi_config_t* dsi_config) {
  return dsi_host_controller_.Config(dsi_config);
}

void DsiDw::DsiImplPowerUp() { dsi_host_controller_.PowerUp(); }

void DsiDw::DsiImplPowerDown() { dsi_host_controller_.PowerDown(); }

void DsiDw::DsiImplSetMode(dsi_mode_t mode) { dsi_host_controller_.SetMode(mode); }

zx_status_t DsiDw::DsiImplSendCmd(const mipi_dsi_cmd_t* cmd_list, size_t cmd_count) {
  return dsi_host_controller_.SendCmd(cmd_list, cmd_count);
}

void DsiDw::DsiImplPhyPowerUp() { dsi_host_controller_.PhyPowerUp(); }

void DsiDw::DsiImplPhyPowerDown() { dsi_host_controller_.PhyPowerDown(); }

void DsiDw::DsiImplPhySendCode(uint32_t code, uint32_t parameter) {
  dsi_host_controller_.PhySendCode(code, parameter);
}

zx_status_t DsiDw::DsiImplPhyWaitForReady() { return dsi_host_controller_.PhyWaitForReady(); }

// static
zx_status_t DsiDw::Create(zx_device_t* parent) {
  zx::result<ddk::PDevFidl> pdev_result = ddk::PDevFidl::Create(parent);
  if (pdev_result.is_error()) {
    zxlogf(ERROR, "Failed to get platform device: %s", pdev_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  ddk::PDevFidl pdev = std::move(pdev_result).value();

  std::optional<fdf::MmioBuffer> dsi_mmio;
  if (zx_status_t status = pdev.MapMmio(0, &dsi_mmio); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map MMIO registers: %s", zx_status_get_string(status));
    return status;
  }
  ZX_ASSERT_MSG(dsi_mmio.has_value(),
                "PDevFidl::MapMmio() succeeded but didn't populate out-param");

  fbl::AllocChecker alloc_checker;
  auto dsi_dw =
      fbl::make_unique_checked<DsiDw>(&alloc_checker, parent, std::move(dsi_mmio).value());
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status =
      dsi_dw->DdkAdd(ddk::DeviceAddArgs("dw-dsi").set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add dw-dsi device: %s", zx_status_get_string(status));
    return status;
  }
  // devmgr now owns the memory for `dsi_dw`
  [[maybe_unused]] DsiDw* const dsi_dw_ptr = dsi_dw.release();

  return ZX_OK;
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return DsiDw::Create(parent); },
};

}  // namespace

}  // namespace dsi_dw

// clang-format off
ZIRCON_DRIVER(dsi_dw, dsi_dw::kDriverOps, "zircon", "0.1");
