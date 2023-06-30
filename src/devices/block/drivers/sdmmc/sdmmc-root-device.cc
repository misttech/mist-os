// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-root-device.h"

#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>

#include <memory>

#include <fbl/alloc_checker.h>

namespace sdmmc {

zx_status_t SdmmcRootDevice::Bind(void* ctx, zx_device_t* parent) {
  ddk::SdmmcProtocolClient host(parent);
  if (!host.is_valid()) {
    zxlogf(ERROR, "failed to get sdmmc protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<SdmmcRootDevice> dev(new (&ac) SdmmcRootDevice(parent, host));

  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate device");
    return ZX_ERR_NO_MEMORY;
  }
  zx_status_t st = dev->DdkAdd("sdmmc", DEVICE_ADD_NON_BINDABLE);
  if (st != ZX_OK) {
    return st;
  }

  [[maybe_unused]] auto* placeholder = dev.release();
  return st;
}

// TODO(hanbinyoon): Simplify further using templated lambda come C++20.
template <class DeviceType>
static zx_status_t MaybeAddDevice(const std::string& name, zx_device_t* zxdev, SdmmcDevice& sdmmc) {
  std::unique_ptr<DeviceType> device;
  if (zx_status_t st = DeviceType::Create(zxdev, sdmmc, &device) != ZX_OK) {
    zxlogf(ERROR, "Failed to create %s device, retcode = %d", name.c_str(), st);
    return st;
  }

  if (zx_status_t st = device->Probe(); st != ZX_OK) {
    return ZX_ERR_WRONG_TYPE;  // Use this to mean probe failure.
  }

  if (zx_status_t st = device->AddDevice(); st != ZX_OK) {
    return st;
  }

  [[maybe_unused]] auto* placeholder = device.release();
  return ZX_OK;
}

void SdmmcRootDevice::DdkInit(ddk::InitTxn txn) {
  SdmmcDevice sdmmc(host_);
  zx_status_t st = sdmmc.Init();
  if (st != ZX_OK) {
    zxlogf(ERROR, "failed to get host info");
    return txn.Reply(st);
  }

  zxlogf(DEBUG, "host caps dma %d 8-bit bus %d max_transfer_size %" PRIu64 "",
         sdmmc.UseDma() ? 1 : 0, (sdmmc.host_info().caps & SDMMC_HOST_CAP_BUS_WIDTH_8) ? 1 : 0,
         sdmmc.host_info().max_transfer_size);

  // Reset the card.
  sdmmc.host().HwReset();

  // No matter what state the card is in, issuing the GO_IDLE_STATE command will
  // put the card into the idle state.
  if ((st = sdmmc.SdmmcGoIdle()) != ZX_OK) {
    zxlogf(ERROR, "SDMMC_GO_IDLE_STATE failed, retcode = %d", st);
    return txn.Reply(st);
  }

  // Probe for SDIO first, then SD/MMC.
  if ((st = MaybeAddDevice<SdioControllerDevice>("sdio", zxdev(), sdmmc)) != ZX_ERR_WRONG_TYPE) {
    return txn.Reply(st);
  }
  if ((st = MaybeAddDevice<SdmmcBlockDevice>("block", zxdev(), sdmmc)) == ZX_OK) {
    return txn.Reply(ZX_OK);
  }

  zxlogf(ERROR, "failed to probe");
  return txn.Reply(st);
}

void SdmmcRootDevice::DdkRelease() { delete this; }

}  // namespace sdmmc

static constexpr zx_driver_ops_t sdmmc_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = sdmmc::SdmmcRootDevice::Bind;
  return ops;
}();

ZIRCON_DRIVER(sdmmc, sdmmc_driver_ops, "zircon", "0.1");
