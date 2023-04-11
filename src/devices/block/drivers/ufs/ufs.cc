// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <safemath/checked_math.h>

#include "src/devices/block/drivers/ufs/ufs_bind.h"

namespace ufs {

zx::result<> Ufs::NotifyEventCallback(NotifyEvent event, uint64_t data) {
  switch (event) {
    // This should all be done by the bootloader at start up and not reperformed.
    case NotifyEvent::kInit:
    // This is normally done at init, but isn't necessary.
    case NotifyEvent::kReset:
    case NotifyEvent::kPreLinkStartup:
    case NotifyEvent::kPostLinkStartup:
    case NotifyEvent::kDeviceInitDone:
    case NotifyEvent::kSetupTransferUtrl:
      return zx::ok();
    // If these get called we're probably in trouble.
    case NotifyEvent::kPrePowerModeChange:
    case NotifyEvent::kPostPowerModeChange:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  };
}

zx_status_t Ufs::AddDevice() {
  if (zx_status_t status =
          DdkAdd(ddk::DeviceAddArgs(kDriverName).set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to run DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Ufs::Bind(void *ctx, zx_device_t *parent) {
  ddk::Pci pci(parent, "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to find PCI fragment");
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::optional<fdf::MmioBuffer> mmio;
  if (zx_status_t status = pci.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to map registers: %s", zx_status_get_string(status));
    return status;
  }

  fuchsia_hardware_pci::InterruptMode irq_mode;
  if (zx_status_t status = pci.ConfigureInterruptMode(1, &irq_mode); status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure interrupt: %s", zx_status_get_string(status));
    return status;
  }
  zxlogf(DEBUG, "Interrupt mode: %u", static_cast<uint8_t>(irq_mode));

  zx::interrupt irq;
  if (zx_status_t status = pci.MapInterrupt(0, &irq); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map interrupt: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = pci.SetBusMastering(true); status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable bus mastering: %s", zx_status_get_string(status));
    return status;
  }
  auto cleanup = fit::defer([&] { pci.SetBusMastering(false); });

  zx::bti bti;
  if (zx_status_t status = pci.GetBti(0, &bti); status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  auto driver = fbl::make_unique_checked<Ufs>(&ac, parent, std::move(pci), std::move(*mmio),
                                              irq_mode, std::move(irq), std::move(bti));
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for UFS driver.");
    return ZX_ERR_NO_MEMORY;
  }
  driver->SetHostControllerCallback(NotifyEventCallback);

  if (zx_status_t status = driver->AddDevice(); status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  [[maybe_unused]] auto placeholder = driver.release();
  cleanup.cancel();
  return ZX_OK;
}

zx_status_t Ufs::Init() {
  // TODO(fxbug.dev/124835): Initialize UFS host controller
  zxlogf(INFO, "Bind Success");
  return ZX_OK;
}

void Ufs::DdkInit(ddk::InitTxn txn) {
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

void Ufs::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (mmio_.get_vmo() != ZX_HANDLE_INVALID) {
    pci_.SetBusMastering(false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.

  delete this;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Ufs::Bind,
};

}  // namespace ufs

ZIRCON_DRIVER(Ufs, ufs::driver_ops, "zircon", "0.1");
