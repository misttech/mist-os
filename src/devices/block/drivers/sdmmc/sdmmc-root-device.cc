// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-root-device.h"

#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

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
// Returns true if device was successfully added. Returns false if the probe failed (i.e., no
// eligible device present).
template <class DeviceType>
static zx::result<bool> MaybeAddDevice(
    const std::string& name, zx_device_t* zxdev, SdmmcDevice& sdmmc,
    const fuchsia_hardware_sdmmc::wire::SdmmcMetadata& metadata) {
  std::unique_ptr<DeviceType> device;
  if (zx_status_t st = DeviceType::Create(zxdev, sdmmc, &device) != ZX_OK) {
    zxlogf(ERROR, "Failed to create %s device, retcode = %d", name.c_str(), st);
    return zx::error(st);
  }

  if (zx_status_t st = device->Probe(metadata); st != ZX_OK) {
    return zx::ok(false);
  }

  if (zx_status_t st = device->AddDevice(); st != ZX_OK) {
    return zx::error(st);
  }

  [[maybe_unused]] auto* placeholder = device.release();
  return zx::ok(true);
}

zx::result<fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>>
SdmmcRootDevice::GetMetadata(fidl::AnyArena& allocator) {
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>(
      parent(), DEVICE_METADATA_SDMMC);
  if (decoded.is_error()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No metadata provided");
      return zx::ok(fidl::ObjectView(allocator,
                                     fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(allocator)
                                         .enable_trim(true)
                                         .enable_cache(true)
                                         .removable(false)
                                         .Build()));
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
      return decoded.take_error();
    }
  }

  // Default to trim and cache enabled, non-removable.
  return zx::ok(fidl::ObjectView(
      allocator, fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(allocator)
                     .enable_trim(!decoded->has_enable_trim() || decoded->enable_trim())
                     .enable_cache(!decoded->has_enable_cache() || decoded->enable_cache())
                     .removable(decoded->has_removable() && decoded->removable())
                     .Build()));
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

  fidl::Arena arena;
  const zx::result metadata = GetMetadata(arena);
  if (metadata.is_error()) {
    return txn.Reply(metadata.status_value());
  }

  // Reset the card.
  sdmmc.host().HwReset();

  // No matter what state the card is in, issuing the GO_IDLE_STATE command will
  // put the card into the idle state.
  if ((st = sdmmc.SdmmcGoIdle()) != ZX_OK) {
    zxlogf(ERROR, "SDMMC_GO_IDLE_STATE failed, retcode = %d", st);
    return txn.Reply(st);
  }

  // Probe for SDIO first, then SD/MMC.
  zx::result<bool> result =
      MaybeAddDevice<SdioControllerDevice>("sdio", zxdev(), sdmmc, **metadata);
  if (result.is_error()) {
    return txn.Reply(result.status_value());
  } else if (*result) {
    return txn.Reply(ZX_OK);
  }
  result = MaybeAddDevice<SdmmcBlockDevice>("block", zxdev(), sdmmc, **metadata);
  if (result.is_error()) {
    return txn.Reply(result.status_value());
  } else if (*result) {
    return txn.Reply(ZX_OK);
  }

  zxlogf(INFO, "failed to probe (no eligible device present)");
  return txn.Reply(ZX_OK);
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
